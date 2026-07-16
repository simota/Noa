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
#   NOA_MODE      throughput | scroll | latency | startup | hold | longevity
#   NOA_SENTINEL  file to create when the workload is done (launcher watches it)
#   NOA_NOWNS     path to the `nowns` monotonic-ns helper
#   NOA_BENCH_CMD (throughput/scroll/longevity) shell command to run, e.g. `cat file`
#   NOA_PROBE     (latency) path to dsr_probe
#   NOA_RESULT    (latency) file dsr_probe writes
#                 "median p95 p99 max min count" into
#   NOA_SAMPLES   (latency, optional) file dsr_probe writes every kept raw
#                 sample into (one ns per line) so the harness can pool
#                 samples across launches
#   NOA_HOLD      (memory/load axes) if "1", after the mode's own work the pty
#                 child sleeps instead of exiting, so the terminal window (and
#                 process tree) stays alive for the harness to sample RSS/CPU
#                 after settling. Terminals that close their window when the
#                 pty child exits would otherwise vanish before sampling.
#   NOA_CYCLES    (longevity) number of flood+idle cycles (default 5)
#   NOA_IDLE_S    (longevity) idle seconds between cycles (default 3)

case "$NOA_MODE" in
  throughput|scroll)
    start=$("$NOA_NOWNS")
    eval "$NOA_BENCH_CMD"
    end=$("$NOA_NOWNS")
    printf '%s %s\n' "$start" "$end" > "$NOA_SENTINEL.part"
    mv "$NOA_SENTINEL.part" "$NOA_SENTINEL"
    ;;
  latency)
    "$NOA_PROBE" "${NOA_PROBE_ITERS:-200}" "${NOA_PROBE_WARMUP:-20}" "$NOA_RESULT" "${NOA_SAMPLES:-}"
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
  *)
    printf 'wrapper: unknown NOA_MODE=%s\n' "$NOA_MODE" >&2
    exit 64
    ;;
esac

if [ "${NOA_HOLD:-0}" = 1 ]; then
  sleep 86400
fi

exit 0
