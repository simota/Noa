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
#   NOA_MODE      throughput | scroll | latency | startup
#   NOA_SENTINEL  file to create when the workload is done (launcher watches it)
#   NOA_NOWNS     path to the `nowns` monotonic-ns helper
#   NOA_BENCH_CMD (throughput/scroll) shell command to run, e.g. `cat file`
#   NOA_PROBE     (latency) path to dsr_probe
#   NOA_RESULT    (latency) file dsr_probe writes "median p99 min count" into

case "$NOA_MODE" in
  throughput|scroll)
    start=$("$NOA_NOWNS")
    eval "$NOA_BENCH_CMD"
    end=$("$NOA_NOWNS")
    printf '%s %s\n' "$start" "$end" > "$NOA_SENTINEL.part"
    mv "$NOA_SENTINEL.part" "$NOA_SENTINEL"
    ;;
  latency)
    "$NOA_PROBE" "${NOA_PROBE_ITERS:-200}" "${NOA_PROBE_WARMUP:-20}" "$NOA_RESULT"
    : > "$NOA_SENTINEL"
    ;;
  startup)
    # Child reached exec == pty ready + window materialized. The launcher's
    # t0..sentinel delta is the warm-start proxy.
    "$NOA_NOWNS" > "$NOA_SENTINEL.part"
    mv "$NOA_SENTINEL.part" "$NOA_SENTINEL"
    ;;
  *)
    printf 'wrapper: unknown NOA_MODE=%s\n' "$NOA_MODE" >&2
    exit 64
    ;;
esac

exit 0
