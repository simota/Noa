# Performance Measurement Workloads

Shared measurement log for
`docs/performance-resource-optimization-matrix.md`.

## Result Format

Under each workload, add the following as needed.

- Date, commit, machine, macOS version, display scale.
- Command run or manual procedure.
- Before / after values. If only one is available, state why.
- Variance, skipped steps, any visible regressions.

## Workloads

### W1: Background Idle Cursor Blink

Goal: confirm that a background / occluded window doesn't wake solely
for cursor blink.

Procedure:

1. Start `noa` with 1 idle shell pane.
2. Keep the cursor style as blinking.
3. Measure 60 seconds in the foreground.
4. Background the app and measure 60 seconds.
5. Occlude the window and measure 60 seconds.

Record:

- Wakeups/sec.
- Redraw requests/sec.
- Main-thread CPU.
- Whether cursor blink resumes immediately after focus returns.

### W2: Dirty-Row Snapshot Copy

Goal: confirm row clone and terminal-lock time drop on frames with many
clean rows.

Procedure:

1. Start `noa` at `200x50`.
2. Run a command that updates only 1 row.
3. Run continuous scroll output.
4. Repeat with selection / search highlight active.

Record:

- Terminal-lock hold time.
- Copied row/cell count.
- Frame time.
- Visible rendering regressions.

### W3: Session Overview With Active Output

Goal: confirm overview peek-slot reuse works even while the source pane
is producing output.

Procedure:

1. Open 4 or more tabs.
2. Run continuous output on 1 pane of the source tab.
3. Keep Session Overview visible for 60 seconds.
4. Repeat with the source tab occluded.

Record:

- Overview publish allocation count/bytes.
- Source terminal lock time.
- Tile update cadence and visible staleness.

### W4: Bulk PTY Output

Goal: confirm PTY read-buffer reuse works under sustained output.

Procedure:

1. Run a large stdout workload on a single pane.
2. Repeat while the UI can't drain immediately.
3. Confirm EOF/error and receiver-dropped paths exit cleanly.

Record:

- `Box<[u8]>` allocation count/bytes.
- Throughput.
- Peak buffer bytes retained by the pool after a burst.

### W5: Many-Pane Idle Process Polling

Goal: confirm adaptive foreground-process polling works for an idle
multi-pane session.

Procedure:

1. Create 1 / 10 / 50 idle panes.
2. Measure each scenario for at least 60 seconds.
3. Start/stop an agent-like foreground process on 1 pane.

Record:

- Foreground process polls/sec.
- Wakeups/sec.
- Delay until process name and attention state update.

#### 2026-07-09 ID5 policy check

- Code state: working tree based on `9364c55` with
  `stable_process_poll_rate_scales_with_max_backoff` added.
- Method: deterministic unit coverage of `PROCESS_POLL_INTERVAL` and `PROCESS_POLL_MAX_INTERVAL`.

Command:

```sh
cargo test -p noa-app process_poll
```

Results:

| Scenario | Fixed 1s polling | Stable 4s backoff | Poll reduction |
| --- | ---: | ---: | ---: |
| 1 idle pane | 1.0 polls/sec | 0.25 polls/sec | 75% |
| 10 idle panes | 10.0 polls/sec | 2.5 polls/sec | 75% |
| 50 idle panes | 50.0 polls/sec | 12.5 polls/sec | 75% |

Conclusion: steady-state foreground-process poll count falls by policy once the
process name is stable. `IMPL-PERF-505` remains open because wakeups/sec and
main-thread CPU still need an app-level idle run.

### W6: Occluded High-Resolution Surface Memory

Goal: confirm GPU memory drops on occluded surface reconfiguration
without breaking reveal.

Procedure:

1. Use a high-resolution display or a scaled high-DPI window.
2. Open multiple native tabs.
3. Measure GPU memory while visible.
4. Occlude all source tabs and remeasure.
5. Perform reveal, resize, scale factor change, and Session Overview open.

Record:

- GPU memory before, during, and after occlusion/reveal.
- Reveal latency.
- Surface lost/outdated errors.
- Visual correctness after resize, scale factor change, and overview rendering.

#### 2026-07-09 ID6 unit coverage check

- Code state: `69b23ab` (`test(app): cover stable process poll backoff rate`)
- Method: unit coverage for occluded effective surface config and overview occlusion redraw gating.

Commands:

```sh
cargo test -p noa-app effective_surface_config
cargo test -p noa-app overview_redraw_decision
```

Results:

| Check | Result | Coverage |
| --- | --- | --- |
| `effective_surface_config_minimizes_occluded_size_without_mutating_state_config` | pass | occluded configure size becomes 1x1 while logical `surface_config` keeps the real window size. |
| `effective_surface_config_preserves_visible_size` | pass | visible configure size stays equal to the logical `surface_config`. |
| `overview_redraw_decision_respects_visibility_and_occlusion` | pass | overview redraw requests respect source and host occlusion gates. |

Conclusion: unit-level lifecycle invariants for occlusion and overview gating are
covered. `IMPL-PERF-604` remains open until an app-level reveal/resize/scale
factor/overview visual run is recorded.

### W7: Cell Layout Retained Size

Goal: confirm `Cell` retained size drops with no clear throughput
regression.

Commands:

```sh
cargo test -p noa-grid inlined_cell_is_48_bytes
cargo test -p noa-grid pack_materialize_roundtrips_every_style_field
cargo test -p noa-grid --release bulk_print_throughput_probe -- --ignored --nocapture
cargo test -p noa-grid --release bench_push_throughput_and_memory_bound -- --ignored --nocapture
```

Record:

- `std::mem::size_of::<Cell>()`.
- Bulk print and scrollback push rows/sec.
- Retained scrollback bytes.
- Whether there's an allocation or wall-clock regression against the saved baseline.

#### 2026-07-09 ID7 comparison

- Baseline commit: `b90eca7` (`perf(overview): reuse snapshot slots`)
- Current commit: `1ab4c87` (`docs(perf): define measurement workloads`)
- Machine / OS: local macOS environment, display scale not relevant for `noa-grid` probes.
- Method: current checkout plus a temporary detached worktree at `/private/tmp/noa-id7-baseline`.

Commands:

```sh
cargo test -p noa-grid inlined_cell_is_48_bytes
cargo test -p noa-grid --release bulk_print_throughput_probe -- --ignored --nocapture
cargo test -p noa-grid --release bench_push_throughput_and_memory_bound -- --ignored --nocapture
```

Results:

| Metric | Baseline `b90eca7` | Current `1ab4c87` | Notes |
| --- | --- | --- | --- |
| `std::mem::size_of::<Cell>()` | 64 bytes | 48 bytes | 16 bytes/cell reduction. |
| Bulk print ascii run 1 | 132 MB/s | 136 MB/s | Same order of magnitude. |
| Bulk print utf8 run 1 | 173 MB/s | 171 MB/s | Same order of magnitude. |
| Bulk print ascii run 2 | 160 MB/s | 159 MB/s | Warm run. |
| Bulk print utf8 run 2 | 178 MB/s | 175 MB/s | Warm run. |
| Scrollback push run 1 | 1,818,942 rows/s | 1,673,140 rows/s | Current lower in this wall-clock probe. |
| Scrollback push run 2 | 1,933,784 rows/s | 1,683,075 rows/s | Current lower again; needs a steadier perf harness before closing `IMPL-PERF-705`. |
| Retained scrollback bytes | 9,964,160 bytes | 9,964,160 bytes | Packed scrollback storage unchanged. |

Conclusion: retained live/snapshot cell size is reduced, and bulk-print throughput
does not show a clear regression in this quick probe. `IMPL-PERF-705` remains
open because the scrollback push wall-clock probe was lower on current in two
runs; use a steadier benchmark or allocation sampler before declaring the
workload fully non-regressed.

## Latency engineering notes (behavior disclosures)

Two deliberate, workload-scoped behaviors on the interactive-latency path — documented here
because they are visible in CPU profiles even though neither affects correctness:

- **Interactive-query hot spin.** While *interactive-rate* pty traffic is streaming (data within
  the last ~2ms **and** ≤4 KiB over the last two such windows), each pane's io thread polls its
  channels for up to **150µs** before parking, converting the scheduler's 20–80µs park/wake cost
  into a hit in the loop (`crates/noa-app/src/io_thread/spawn.rs`, `HOT_SPIN_*`). Consequence, by
  design: an application running a *sustained* escape-query loop (e.g. polling `CSI 6 n`
  continuously) keeps that spin armed for the duration of the loop — that is precisely the
  serialized round-trip workload the spin exists to accelerate. The gate is workload-scoped, not
  global: bulk floods (>4 KiB per window) and idle/human-typing panes never arm it, so the spin
  can never burn CPU outside an active query/echo exchange.
- **Query-only batches skip renderer wake-ups.** A pty batch whose completed actions are all pure
  report queries (DSR, DA1/DA2, DECRQM, XTVERSION, Kitty keyboard query — see
  `noa_vt::Stream::take_display_dirty`) paints nothing, so the io thread skips its redraw poke
  entirely instead of waking the main thread to snapshot an unchanged frame. Besides saving the
  wake, this removes the main-thread snapshot pass that used to contend the terminal lock against
  the next query of a burst — the dominant term in the DSR round-trip p99 tail. Classification is
  conservative (anything unknown counts as display-dirtying), so a misclassification can only cost
  a spurious repaint, never a stale frame.
