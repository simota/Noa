# Bench axis: `fire` — DOOM-Fire IO stress (design)

Status: IMPLEMENTED (2026-07-17). Adds a 7th axis to `bench/run_all.sh`
reproducing the workload class of
[DOOM-fire-zig](https://github.com/const-void/DOOM-fire-zig) — the de-facto
community IO benchmark for terminals (Ghostty's release notes cite it).

## Motivation

DOOM-fire-zig fps figures are widely published for many terminals (various
machines/window sizes, **not** comparable to ours — see Fairness).

What it exercises that the existing axes do not: a **frame-structured,
truecolor-dense, cursor-repositioning flood** — every frame repaints the
whole region with per-cell `SGR 38;2/48;2` RGB and half-block glyphs, under
pty flow control. `throughput` streams flat text; `scroll` exercises
scroll-regions; `fire` is the "animated TUI at max rate" shape (fps = drain
rate of full-screen repaints). It cleanly detects display-paced consumption:
such terminals plateau at ~display-refresh fps regardless of hardware.

## Decision 1 — reimplement in C, don't vendor the Zig binary

`bench/tools/fire.c`, built by the existing `cc -O2` tools block. Rationale:

- The harness's tools are all single-file C built on the fly; requiring a
  Zig toolchain (or vendoring an opaque prebuilt binary) breaks the
  "third party re-runs everything with one command" property.
- Upstream DOOM-fire-zig has no fixed-duration mode and no machine-readable
  output (fps is drawn on screen, runs until keypress) — automation would
  need a fork anyway.
- The algorithm is tiny and public (Fabien Sanglard's DOOM fire): a 1-byte
  heat buffer, per-frame decay/spread, 37-entry palette mapped to RGB,
  rendered as `▄` half-blocks (fg = lower pixel, bg = upper pixel, 2 fire
  rows per cell row).

The port must match the upstream *workload shape* (truecolor half-block
full-region repaint per frame), not its exact bytes. Published fps numbers
are anchors for manual runs of the real DOOM-fire-zig, never pasted into our
results.

## Decision 2 — render region follows the geometry mode (revised 2026-07-17)

fps is inversely proportional to cell count, so the region choice is a
fairness decision:

- **Fullscreen runs (the harness default since fullscreen measurement
  landed): full-window** — matches upstream DOOM-fire-zig's official
  condition. Every terminal fills the same physical screen; the cell count
  then follows each terminal's font defaults (as in upstream comparisons)
  and is disclosed per rep (`region` in raw.tsv, parenthesized in table.md).
  The fullscreen gate guarantees the `TIOCGWINSZ` read happens at final
  geometry.
- **Windowed fallback runs: fixed 80×24 cell region** (fire buffer 80×48
  pixels, top-left anchored via absolute `CUP`) — on unequal window
  geometry, full-window fps would measure the geometry lottery, not the
  terminal. The fixed region fits every default grid (no clipping) and
  gives **every terminal a byte-identical stream**.

Fixed PRNG seed → the frame sequence is deterministic across terminals and
runs in both modes. The two conditions are mutually incomparable; every
results dir records which one ran (`fire_condition`).

## Decision 3 — producer-side fps under flow control (same proxy as axis 1)

The pty has a small kernel buffer with flow control: the producer's `write`
blocks until the terminal drains. Frames written per second therefore ≈
frames consumed per second — the same "consume the pipe" proxy the
throughput axis already documents. No screen-capture or vsync instrumentation
is attempted.

`fire.c <secs> <result-file>`:

1. raw mode, enter alt screen (`CSI ?1049h`), hide cursor.
2. **Warmup: 60 frames, discarded** (glyph-atlas population, palette ramp,
   alt-screen entry transients).
3. Render frames flat-out for `<secs>` (`CLOCK_MONOTONIC` bracket), counting
   completed frames. Each frame is composed into one buffer and written with
   a single retry-on-partial `write` loop.
4. Leave alt screen, restore tty.
5. Write `"<frames> <elapsed_ns> <fps> <cols>x<rows>"` to the result file.

## Integration

### `bench/wrapper.sh` — new mode

```sh
fire)
  "$NOA_FIRE" "${NOA_FIRE_SECS:-10}" "$NOA_RESULT"
  : > "$NOA_SENTINEL"
  ;;
```

New env keys: `NOA_FIRE` (tool path), `NOA_FIRE_SECS`. Documented in the
header env contract. `NOA_HOLD` composes as usual (unused by this axis).

### `bench/run_all.sh`

- `AXES` default set gains `fire` (opt-out via `--axes` as with every axis).
- Params: full `FIRE_REPS=3`, `FIRE_SECS=10`; quick `FIRE_REPS=1`,
  `FIRE_SECS=3`. Timeout `FIRE_TIMEOUT=60`.
- Tools block: add `fire` to `tools_fresh` + the `cc -O2` build line.
- Axis loop mirrors latency's result-file pattern:
  `run_once "$term" fire "$FIRE_TIMEOUT"` → read result file → emit per-rep
  `fps`, `frames`, `elapsed_ns`, plus `median fps`. Median of reps is the
  headline (fps is higher-is-better, like `mib_per_s`).
- **Focus each terminal during the run** (reuse the latency axis's
  `activate_term` schedule, PID-scoped): display-paced terminals throttle
  unfocused/occluded windows, which would understate them; focused is the
  representative condition and is applied uniformly.
- run_once needs a one-line extension: trigger the activation schedule for
  `mode = latency or fire`.
- Contention: CPU-bound axis — already covered by the builder-quiescence
  gate and loadavg bookends.
- `--equalize` mode: skip (the fixed 80×24 region makes the workload
  grid/font-independent by construction; font rendering differences remain
  part of each terminal's own cost, as with scroll).

### `bench/aggregate.py` / `table.md`

- New axis row: `fire — DOOM-fire proxy (fps, median of N×10 s)`, ranked
  higher-is-better on median fps.
- `results.json`: per-terminal `{fps_median, fps_reps[], frames, region,
  winsize}`.

### `bench/METHODOLOGY.md`

New "Axis 7: fire" section covering: lineage (DOOM-fire-zig / Sanglard
algorithm), the fixed-region fairness decision, the flow-control fps proxy
and its caveat (drain rate, not photon rate), the focus policy, and an
explicit note that published DOOM-fire-zig figures are other machines +
full-window regions and must not be compared to this axis's numbers.

## Acceptance criteria

1. `bench/run_all.sh --axes fire` produces per-terminal median fps for all
   installed terminals with zero UNMEASURED rows on a quiet machine.
2. `raw.tsv` contains per-rep `fps/frames/elapsed_ns` and the winsize note.
3. Byte-stream identity: running `fire.c` twice redirected to a file yields
   identical output (deterministic seed) — verifiable with `cmp`.
4. A visual spot check in noa shows the fire animating in the top-left
   80×24 region (alt screen, restored cleanly on exit).
5. Rankings appear in `table.md` with fps ranked higher-is-better.
6. The EXIT-trap / kill_term lifecycle leaves no processes behind (existing
   invariant, unchanged).

## Non-goals

- Matching upstream DOOM-fire-zig's exact escape output or its published fps.
- Photon-to-glass frame-rate measurement (out of scope for the whole
  harness; see METHODOLOGY axis 2 caveats).
- Window-size-scaled numbers in the scored axis. A manual full-window mode
  exists for upstream-style anchor runs (`fire <secs> <result> full`, or
  `FIRE_FULL=1 bench/run_benchmark.sh fire`) — fps scales ~1/cell-count
  (e.g. a 128×37 window has ~2.5× the cells of the fixed 80×24 region, so
  fixed-region fps reads ~2.5× higher than full-window fps on the same
  terminal), which is exactly why the scored axis pins the region: full-mode
  numbers depend on window geometry and never enter ranking.
