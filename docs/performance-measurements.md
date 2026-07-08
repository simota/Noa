# Performance Measurement Workloads

`docs/performance-resource-optimization-matrix.md` の共通測定ログ。

## Result Format

各 workload の下に、必要に応じて以下を追記する。

- 日付、commit、machine、macOS version、display scale。
- 実行コマンドまたは手動手順。
- before / after 値。片方しか無い場合はその理由。
- ばらつき、skip した手順、見た目の回帰有無。

## Workloads

### W1: Background Idle Cursor Blink

目的: background / occluded window が cursor blink だけで wake しないことを確認する。

手順:

1. idle shell 1 pane で `noa` を起動する。
2. cursor style は blinking のままにする。
3. foreground で 60 秒測る。
4. app を background にして 60 秒測る。
5. window を occlude して 60 秒測る。

記録:

- wakeups/sec。
- redraw requests/sec。
- main-thread CPU。
- focus 復帰後に cursor blink が即時再開するか。

### W2: Dirty-Row Snapshot Copy

目的: clean 行が多い frame で row clone と terminal lock time が下がることを確認する。

手順:

1. `200x50` で `noa` を起動する。
2. 1 行だけ更新する command を流す。
3. 連続 scroll output を流す。
4. selection / search highlight ありで繰り返す。

記録:

- terminal lock hold time。
- copied row/cell count。
- frame time。
- visible rendering regression。

### W3: Session Overview With Active Output

目的: source pane が出力中でも overview peek-slot reuse が効くことを確認する。

手順:

1. 4 tab 以上を開く。
2. source tab の 1 pane で continuous output を流す。
3. Session Overview を 60 秒表示し続ける。
4. source tab が occluded の状態でも繰り返す。

記録:

- overview publish allocation count/bytes。
- source terminal lock time。
- tile update cadence と visible staleness。

### W4: Bulk PTY Output

目的: sustained output で PTY read-buffer reuse が効くことを確認する。

手順:

1. single pane で large stdout workload を流す。
2. UI が即時 drain できない状態でも繰り返す。
3. EOF/error、receiver dropped の経路が clean exit することを確認する。

記録:

- `Box<[u8]>` allocation count/bytes。
- throughput。
- burst 後に pool が保持する buffer bytes の peak。

### W5: Many-Pane Idle Process Polling

目的: idle multi-pane session で adaptive foreground-process polling が効くことを確認する。

手順:

1. 1 / 10 / 50 idle pane を作る。
2. 各 scenario を最低 60 秒測る。
3. 1 pane で agent-like foreground process を start/stop する。

記録:

- foreground process polls/sec。
- wakeups/sec。
- process name と attention state が更新されるまでの遅延。

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

目的: occluded surface reconfiguration で GPU memory が下がり、reveal が壊れないことを確認する。

手順:

1. high-resolution display または scaled high-DPI window を使う。
2. native tab を複数開く。
3. visible 状態の GPU memory を測る。
4. source tab をすべて occlude して再測定する。
5. reveal、resize、scale factor change、Session Overview open を実行する。

記録:

- occlusion 前、中、reveal 後の GPU memory。
- reveal latency。
- Surface lost/outdated error。
- resize、scale factor change、overview rendering 後の visual correctness。

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

目的: `Cell` retained size が下がり、明確な throughput regression がないことを確認する。

Commands:

```sh
cargo test -p noa-grid inlined_cell_is_48_bytes
cargo test -p noa-grid pack_materialize_roundtrips_every_style_field
cargo test -p noa-grid --release bulk_print_throughput_probe -- --ignored --nocapture
cargo test -p noa-grid --release bench_push_throughput_and_memory_bound -- --ignored --nocapture
```

記録:

- `std::mem::size_of::<Cell>()`。
- bulk print と scrollback push の rows/sec。
- retained scrollback bytes。
- 保存済み baseline と比べた allocation または wall-clock regression の有無。

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
