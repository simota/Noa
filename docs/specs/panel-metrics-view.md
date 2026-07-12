# panel-metrics-view

- slug: panel-metrics-view
- title: Per-pane running process / CPU / memory list view (process monitor)
- status: locked (2026-07-11)
- owner: simota
- build-path: apex (single unattended run. fallback: feature / orbit)

## L0 — Vision

`noa` keeps an independent pty session per tab/split pane, but there is no way to see across panes what's currently running and how much resource it's consuming. When running many agents (Claude Code/codex, etc.) side by side, a view that lists per-pane running process name, CPU usage, and memory usage is needed to identify runaway/high-load sessions and decide how to triage them.

- Target: users who run many agents/builds side by side in noa (= the author themself)
- Job: identify a high-load/runaway pane within seconds and get the information needed to decide on a response (kill/cleanup)
- Success: all live panes' process/CPU/mem can be listed with 1s freshness, and a high-load pane can be identified within seconds
- Assumption: macOS-first (non-macOS shows all columns as "—" as a degraded fallback)
- Measurement target: **the foreground process tree** (foreground process group + its descendants) — ratified

## Reuse / constraints (Lens scan results)

### Reusable assets
- `ForegroundProcessProbe` (noa-pty/src/pty.rs:213) — already holds a dup'd fd, is Send, and already polls in the background. Starting point for extending `poll_metrics()`
- `branch_poll.rs` metadata worker — 1–4s adaptive polling, a probe map per `SessionCardId`. Where the metrics tick gets added
- `SessionStore` + `SessionDelta` (session_store.rs) — add `SessionDelta::Metrics`, mirroring `Process` (:582)
- `SessionCard` (session_store.rs:130) — add a metrics field alongside `process`
- Sidebar row model/render (sidebar.rs:138, app/sidebar/render.rs)
- macOS syscall template: tcgetpgrp + proc_name + sysctl(KERN_PROCARGS2) (pty.rs:270-324)

### Constraints
- Collection must not happen on the io read loop or UI thread → do it in the branch_poll worker
- winit/wgpu are confined to noa-app (+noa-render). The overlay uses the wgpu snapshot approach
- CPU% needs the delta between two samples → resolved by a fixed 1s schedule while displayed
- No `sysinfo` dependency (current code follows the hand-rolled libc convention). `kinfo_proc` is not defined in the libc crate, so sysctl(KERN_PROC_ALL) is not adopted
- Pure-logic/GUI separation is repo convention (pure model + snapshot)

## Assumption Ledger
- ASSUME-1 (ratified): CPU% denominator is "1 core = 100%" (Activity Monitor/top style; multithreaded can exceed 100%)
- ASSUME-2 (ratified): memory is the physical footprint (`ri_phys_footprint`) summed across the tree
- ASSUME-3 (ratified, revised): all columns show "—" on non-macOS (since the existing probe doesn't return a process name either; the row itself is still shown)
- ASSUME-4 (resolved): a launch keybinding is out of scope for v1 (command palette entry only). Future work once the keybind config foundation is in place
- ASSUME-5 (ratified): row filtering/search is not needed for v1 (assuming pane counts in the tens)

## Direction decision (CHALLENGE)

- **Adopted: B — dedicated "process monitor" overlay** (sortable table-form modal + jump to pane) — ratified
- Considered but rejected:
  - A: sidebar extension — cards get overcrowded, no sorting, invisible when sidebar is hidden (the data layer is shared, so this can be added on top later)
  - C: Tab Overview integration — tiles suit visual confirmation but not numeric comparison/sorting
  - D: hybrid (B + lightweight A warning) — too much scope for v1. Can be built on top of B's data layer later

## SHAPE — proposal (ratified)

- Working name: **Process Monitor** overlay
- A wgpu modal of the same shape as the command palette/theme settings. Lists every live pane across all windows and all tabs, one row per pane, in table form
- Columns: process name / CPU% / memory / process count / elapsed time / affiliation (tab name, pane position). Values are summed over the foreground process tree
- Operations: ↑↓ to select, Enter to jump, Esc to close. A key cycles the sort column. No destructive operations
- Collection: only while displayed, fixed 1s interval, branch_poll worker

## L1 — Requirements

Functional requirements:
- **FR-1** The process monitor can be opened/closed from a command palette entry (a dedicated keybinding is out of scope)
- **FR-2** Lists every live pane across all windows and all tabs, one row per pane
- **FR-3** Each row shows process name / CPU% / memory / process count / elapsed time / affiliation (tab name, pane position)
- **FR-4** CPU/memory/process count are the summed values over the foreground process tree (all processes belonging to the foreground pgid ∪ its descendants). CPU% is on a 1-core = 100% basis
- **FR-5** Default sort is CPU descending. The sort key can cycle CPU (descending) → memory (descending) → process name (ascending)
- **FR-6** ↑↓ moves row selection, Enter jumps to the corresponding pane (focuses the window/tab/pane and closes the overlay), Esc closes it
- **FR-7** Metrics collection (enumerating processes + rusage-family calls) happens only while the overlay is displayed, at a fixed 1s interval. None of these calls are issued while hidden (the existing adaptive process-name polling continues as before)
- **FR-8** When a value cannot be obtained (non-macOS / process gone / before the first CPU% sample), show "—" without crashing or dropping the row. Non-macOS shows "—" in every column

Non-functional requirements:
- **NFR-1** Collection happens on the branch_poll worker thread (no io read loop, no UI thread)
- **NFR-2** Process enumeration happens once per tick and the result is shared across all panes (design target: under 50ms per tick at ~30 panes — a reference figure; the AC only verifies a single enumeration)
- **NFR-3** Sort/formatting/selection/row-construction pure logic lives in a GUI-independent module and is unit-testable
- **NFR-4** No new heavyweight dependency (e.g. sysinfo) is added. Follow the existing direct-libc-call convention
- **NFR-5** winit/wgpu dependencies stay confined to noa-app / noa-render (existing convention)

## L2 — Detail

### Collection layer (noa-pty + branch_poll)
- Extend `ForegroundProcessProbe` with `poll_metrics(&mut self, snapshot: &ProcSnapshot) -> Option<PaneMetrics>`
- **Process snapshot** (once per tick, shared across all panes): enumerate all pids via `proc_listallpids`, and get ppid/pgid/start time for each pid via `proc_pidinfo(PROC_PIDTBSDINFO)` (`proc_bsdinfo`: `pbi_ppid` / `pbi_pgid` / `pbi_start_tvsec`), both already defined in the libc crate
- **Tree construction**: for the foreground pgid from `tcgetpgrp(fd)`, take all processes with `pbi_pgid == pgid` ∪ their descendants (walking ppid). Must not miss grandchildren reparented to launchd after an intermediate parent exits, nor surviving members after the pgid leader dies
- **Per-pid measurement**: get `ri_user_time + ri_system_time` (CPU time, mach ticks → ns conversion) and `ri_phys_footprint` (memory) via `proc_pid_rusage(RUSAGE_INFO_V4)`
- CPU% = (tree-summed CPU time delta from the previous tick) ÷ (elapsed wall time), on a 1-core = 100% basis. The first sample is "—". A pid that disappeared between ticks is treated as 0 and summation continues
- Elapsed time = anchored at the pgid leader's `pbi_start_tvsec`. If the leader has died, fall back to the oldest process's start time within the group; if none, "—"
- `PaneMetrics { cpu_permille: Option<u32>, mem_bytes: u64, proc_count: u32, started_at: Option<SystemTime> }`. When unobtainable for a pane, `SessionDelta::Metrics { metrics: None }` (all columns "—")
- Add `ProbeControl::MetricsActive(bool)` to branch_poll. Metrics ticks (fixed 1s, on a schedule independent from the existing adaptive process-name polling) only run while true. Results are posted as `SessionDelta::Metrics { id, metrics: Option<PaneMetrics> }` (mirroring the `SessionDelta::Process` pattern at :582)
- Non-macOS: `poll_metrics` always returns `None` (same degradation as the existing probe)

### State layer (session_store)
- Add `metrics: Option<PaneMetrics>` to `SessionCard`. Updated by applying `SessionDelta::Metrics`
- Clear metrics on all cards when the overlay closes (prevents re-displaying stale values)

### UI layer (noa-app + noa-render)
- New pure-logic module `crates/noa-app/src/process_monitor.rs`: implements the row model (SessionStore → row list construction), sort state, selection state, and value formatters, GUI-independent
  - Formatters: CPU% as an integer percentage (can exceed 100%), memory in auto-scaled MB/GB, elapsed time as `mm:ss`, `hh:mm:ss` once past one hour
- Rendering follows the same shape as the command palette/theme settings: add a snapshot type to `noa_render` and draw a wgpu overlay (following the existing modal-addition boilerplate — palette registration, input routing, snapshot, rendering, Esc handling)
- Affiliation column = window/tab title + pane position (reusing existing SessionCard metadata)
- Jump = reuses the existing sidebar/Overview pane-focus path

## L3 — Acceptance Criteria

| ID | What is verified | Verification method | Requirement |
|----|---------|---------|---------|
| AC-1 | A "Process Monitor" entry exists in the command palette; running it opens the overlay, and Esc closes it | manual | FR-1, FR-6 |
| AC-2 | With multiple windows × tabs × split panes, the row count matches the total number of live panes | unit test (row construction) | FR-2 |
| AC-3 | With `yes > /dev/null` running in a pane, that pane's row shows CPU% ≥ 90% within 2s (±1 tick) | manual (live check) | FR-3, FR-4, FR-7 |
| AC-4 | Load that escapes into a child process, e.g. `sh -c 'yes > /dev/null'`, is still reflected in the summed CPU% | manual | FR-4 |
| AC-5 | The memory column shows the tree-summed value in auto-scaled MB/GB | formatter unit test + manual | FR-3, FR-4 |
| AC-6 | Initial display is CPU descending. The sort key cycles memory (descending) → process name (ascending) → CPU (descending), and the order follows | unit test | FR-5 |
| AC-7 | ↑↓ moves selection, and Enter focuses the corresponding pane's window/tab/pane and closes the overlay | manual | FR-6 |
| AC-8 | While `MetricsActive=false`, the metrics collection path (enumeration, rusage) is not called on the branch_poll worker's tick. Collection happens only on the branch_poll worker | worker unit test + code review | FR-7, NFR-1 |
| AC-9 | While displayed, values update at a 1s±0.5s interval (CPU% becomes numeric from the second tick onward) | manual | FR-7 |
| AC-10 | On the tick right after the foreground process group exits, the corresponding row shows "—" without panicking (including row construction against a snapshot containing a gone pid) | unit test | FR-8 |
| AC-11 | The pure logic for sorting, selection, formatting (mm:ss / hh:mm:ss rollover included), and row construction is verified by `cargo test -p noa-app` | unit test | NFR-3 |
| AC-12 | Process enumeration (`proc_listallpids` + `proc_pidinfo`) happens once per tick, and all panes share the same snapshot | unit test (snapshot sharing) + code review | NFR-2 |
| AC-13 | The row model holds all six fields (process name / CPU% / memory / process count / elapsed time / affiliation), and affiliation is built from tab name + pane position | unit test | FR-3 |
| AC-14 | Code review confirms no new dependency was added (Cargo.toml diff) and that wgpu/winit dependencies stay confined to noa-app/noa-render | code review | NFR-4, NFR-5 |
| AC-15 | On a non-macOS target, `poll_metrics` returns `None` and the row displays "—" in every column (unit test behind the cfg gate) | unit test | FR-8 |

## Scope

**In-scope**: everything in the FR/NFR above (real measurement on macOS, degraded "—" display on non-macOS).
**Out-of-scope**: destructive operations such as kill / history/graph display / disk & network I/O / real measurement on non-macOS / dedicated keybinding (pending keybind config foundation) / sidebar high-load warning (future option D) / expanding individual processes within a pane into separate rows / row filtering/search.

## Open Questions / Deferred Decisions

- Default assignment of a dedicated keybinding (once the keybind config foundation is in place; v1 is command-palette only)
- Sidebar high-load warning indicator (option D) — can be built on top of this spec's data layer (`SessionDelta::Metrics`), but requires a decision on switching to always-on collection
- The NFR-2 50ms target is a design reference figure (not covered by an AC). Consider turning it into a benchmark AC if a real-world issue shows up

## Quality gate results (2026-07-11)

Judge spec review: **GATE_PASS** (high 0 / medium 8 / low 9). All medium findings have been folded into the draft: dropped kinfo_proc in favor of proc_listallpids, corrected the pgid tree definition, all-"—" display on non-macOS, moved the keybinding out of scope, added ACs for FR-3/NFR-1/NFR-4/5 (AC-13–15), folded AC-8 into NFR-1, and downgraded NFR-2 to a reference figure.
