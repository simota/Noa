#!/bin/sh
# wrapper.sh — the pty child that every terminal-under-test executes.
#
# Uniform workload driver across all terminals: for terminals with an exec flag
# (ghostty -e, kitty --hold) it is passed as the command; for terminals without
# one (noa, termy) it is installed as $SHELL so the terminal spawns it as the
# login shell. Either way the *same* process runs inside every terminal, so the
# only cross-terminal difference is the launch mechanism, never the workload.
#
# All positional args (e.g. the login `-l` that noa passes) are intentionally
# ignored. Behavior is selected entirely by NOA_MODE + friends in the env.
#
# Env contract:
#   NOA_MODE      throughput | scroll | latency | startup | hold | longevity | fire
#   NOA_SENTINEL  file to create when the workload is done (launcher watches it)
#   NOA_NOWNS     path to the `nowns` monotonic-ns helper
#   NOA_BENCH_CMD (throughput/scroll/longevity) shell command to run, e.g. `cat file`
#   NOA_PROBE     (latency) path to dsr_probe
#   NOA_RESULT    (latency) file dsr_probe writes
#                 "median p95 p99 max min count" into
#   NOA_SAMPLES   (latency, optional) file dsr_probe writes every kept raw
#                 sample into (one ns per line) so the harness can pool
#                 samples across launches
#   NOA_FIRE      (fire) path to the `fire` DOOM-fire IO-stress tool
#   NOA_FIRE_SECS (fire) measured duration in seconds (after 60 warmup frames)
#   NOA_FIRE_ARG  (fire, optional) "full" = render to the live window size
#                 (upstream DOOM-fire condition; the harness sets it on
#                 fullscreen runs). Empty = fixed 80x24 region.
#   NOA_GO        (workload modes, optional) gate file: when set, the
#                 workload starts only after the file appears — the harness
#                 creates it once the window reached its measurement
#                 geometry (fullscreen). 10s fallback so a lost gate can
#                 never hang the child forever.
#   NOA_HOLD      (memory/load axes) if "1", after the mode's own work the pty
#                 child sleeps instead of exiting, so the terminal window (and
#                 process tree) stays alive for the harness to sample RSS/CPU
#                 after settling. Terminals that close their window when the
#                 pty child exits would otherwise vanish before sampling.
#   NOA_CYCLES    (longevity) number of flood+idle cycles (default 5)
#   NOA_IDLE_S    (longevity) idle seconds between cycles (default 3)
#   NOA_MODE=latload (latency-under-load axis, bench/METHODOLOGY.md axis 8):
#     runs a no-load DSR control, then starts a bounded-duration bulk
#     producer against the SAME pty and runs a second DSR probe while it is
#     still actively writing — the pairing (throughput, latency-under-load)
#     is the whole point of the axis.
#     NOA_BULK             path to the bulk_produce tool
#     NOA_BULK_FILE        file bulk_produce streams in a loop
#     NOA_BULK_DURATION    seconds bulk_produce keeps writing (bounded, not
#                           byte-count-bounded — see bulk_produce.c)
#     NOA_BULK_RESULT       file bulk_produce writes "<bytes> <elapsed_ns>" to
#     NOA_BULK_PROGRESS    (optional) file bulk_produce appends
#                           "<ts_ns> <cumulative_bytes>" to every ~200ms, so
#                           the harness can verify the producer was still
#                           writing during the load-probe's own window
#     NOA_LOAD_WARMUP_DISCARD_S  seconds to let the producer ramp before the
#                           load probe starts sampling
#     NOA_CTRL_ITERS/NOA_CTRL_WARMUP  no-load control probe iteration counts
#     NOA_PROBE_ITERS/NOA_PROBE_WARMUP  in-load probe iteration counts
#     NOA_RESULT_NOLOAD/NOA_SAMPLES_NOLOAD  control probe outputs
#     NOA_RESULT/NOA_SAMPLES               in-load probe outputs (same vars
#                           the plain `latency` mode uses)
#     NOA_OVERLAP_MARKER   file wrapper writes "<probe_start_ns> <probe_end_ns>"
#                           to (CLOCK_MONOTONIC via nowns) — the load probe's
#                           own wall-clock window, checked against
#                           NOA_BULK_PROGRESS to verify overlap actually
#                           happened rather than assuming it from launch order

# wait_go — block until the harness signals final window geometry (NOA_GO
# file appears). No-op when NOA_GO is unset (memory/load scenarios reuse the
# workload modes without gating). Bounded so a lost gate cannot hang the run.
wait_go() {
  [ -n "${NOA_GO:-}" ] || return 0
  i=0
  while [ ! -f "$NOA_GO" ] && [ "$i" -lt 200 ]; do
    sleep 0.05
    i=$((i + 1))
  done
}

case "$NOA_MODE" in
  throughput|scroll)
    wait_go
    start=$("$NOA_NOWNS")
    eval "$NOA_BENCH_CMD"
    end=$("$NOA_NOWNS")
    printf '%s %s\n' "$start" "$end" > "$NOA_SENTINEL.part"
    mv "$NOA_SENTINEL.part" "$NOA_SENTINEL"
    ;;
  latency)
    wait_go
    "$NOA_PROBE" "${NOA_PROBE_ITERS:-200}" "${NOA_PROBE_WARMUP:-20}" "$NOA_RESULT" "${NOA_SAMPLES:-}"
    : > "$NOA_SENTINEL"
    ;;
  fire)
    wait_go
    # DOOM-fire IO stress (docs/specs/bench-doom-fire.md): renders truecolor
    # half-block fire flat-out for NOA_FIRE_SECS under pty flow control —
    # full-window when NOA_FIRE_ARG=full (fullscreen runs; wait_go above
    # guarantees the winsize read happens at final geometry), fixed 80x24
    # otherwise. Writes "<frames> <elapsed_ns> <fps> <winsize> <region>"
    # into NOA_RESULT.
    "$NOA_FIRE" "${NOA_FIRE_SECS:-10}" "$NOA_RESULT" ${NOA_FIRE_ARG:-}
    : > "$NOA_SENTINEL"
    ;;
  startup)
    # Child reached exec == pty ready + window materialized. The launcher's
    # t0..sentinel delta is the warm-start proxy.
    "$NOA_NOWNS" > "$NOA_SENTINEL.part"
    mv "$NOA_SENTINEL.part" "$NOA_SENTINEL"
    ;;
  hold)
    # No workload — just signal ready. Used for mem-idle/mem-multitab, where
    # the harness only needs the window alive to sample later (NOA_HOLD=1).
    : > "$NOA_SENTINEL"
    ;;
  longevity)
    # Repeated flood(scroll/throughput workload)+idle cycles in ONE pty-child
    # lifetime, so the sampled RSS trajectory reflects the same long-lived
    # process (a relaunch-per-cycle would measure cold start N times, not
    # longevity growth). Writes "$NOA_SENTINEL.cycle$i" after each cycle's
    # flood + idle settle so the harness can sample in between.
    n="${NOA_CYCLES:-5}"; idle="${NOA_IDLE_S:-3}"
    i=1
    while [ "$i" -le "$n" ]; do
      eval "$NOA_BENCH_CMD"
      sleep "$idle"
      : > "$NOA_SENTINEL.cycle$i"
      i=$((i + 1))
    done
    : > "$NOA_SENTINEL"
    ;;
  latload)
    wait_go
    # 1. no-load control: baseline echo latency with nothing else running,
    #    in the SAME launch/session as the load probe below (same-conditions
    #    pairing — see METHODOLOGY.md axis 8 "Report the pairing").
    "$NOA_PROBE" "${NOA_CTRL_ITERS:-300}" "${NOA_CTRL_WARMUP:-30}" \
      "$NOA_RESULT_NOLOAD" "${NOA_SAMPLES_NOLOAD:-}"
    # 2. start the bulk producer against the SAME pty, in the background.
    "$NOA_BULK" "${NOA_BULK_DURATION:-20}" "$NOA_BULK_FILE" "$NOA_BULK_RESULT" \
      "${NOA_BULK_PROGRESS:-}" &
    bulk_pid=$!
    # 3. let contention ramp up before sampling (discard the startup
    #    transient of the producer's write loop reaching steady pty
    #    backpressure — a fraction of a second in practice; the seconds-long
    #    discard here is a deliberate margin, not the unrelated 5-40s
    #    GPU-driver-pool reclaim window documented for the memory axes).
    sleep "${NOA_LOAD_WARMUP_DISCARD_S:-2}"
    # 4. echo latency WHILE the bulk producer is actively writing. The
    #    wrapper's own before/after timestamps are recorded so the harness
    #    can check the producer's progress log against this exact window.
    probe_start=$("$NOA_NOWNS")
    "$NOA_PROBE" "${NOA_PROBE_ITERS:-200}" "${NOA_PROBE_WARMUP:-20}" \
      "$NOA_RESULT" "${NOA_SAMPLES:-}"
    probe_end=$("$NOA_NOWNS")
    if [ -n "${NOA_OVERLAP_MARKER:-}" ]; then
      printf '%s %s\n' "$probe_start" "$probe_end" > "$NOA_OVERLAP_MARKER"
    fi
    wait "$bulk_pid" 2>/dev/null
    : > "$NOA_SENTINEL"
    ;;
  *)
    printf 'wrapper: unknown NOA_MODE=%s\n' "$NOA_MODE" >&2
    exit 64
    ;;
esac

if [ "${NOA_HOLD:-0}" = 1 ]; then
  sleep 86400
fi

exit 0
