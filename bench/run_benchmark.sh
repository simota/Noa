#!/bin/bash
# run_benchmark.sh — manual in-terminal benchmark.
#
# Runs any subset of the suite's workloads INSIDE the terminal you execute it
# from: run_all.sh measures terminals from the outside (fresh process, config
# isolation, PID-scoped lifecycle) and is the source of scored numbers; this
# script is the quick self-check you run by hand in whatever terminal you are
# sitting in. Same workload files and tools, no isolation, no ranking.
#
# Usage:
#   bench/run_benchmark.sh                  # all tests, prompted one by one
#   bench/run_benchmark.sh ascii fire       # just these tests
#   bench/run_benchmark.sh --yes scroll     # no [Enter] prompts
#   bench/run_benchmark.sh --list           # list available tests
#
# Tests:
#   ascii    cat 150MB_ascii.txt      (throughput, plain text)
#   unicode  cat 150MB_unicode.txt    (throughput, CJK/emoji/CSI mix)
#   scroll   cat scroll_stress.txt    (SGR churn + scroll regions, ~40MB)
#   fire     DOOM-fire IO stress      (fixed 80x24 truecolor repaint, fps)
#   latency  DSR ESC[6n round-trip    (parser-responsiveness proxy, µs)
#
# Env knobs: FIRE_SECS (default 10). The fire test renders to the LIVE
#            window size by default (upstream DOOM-fire-zig's full-window
#            condition, same as the harness's fullscreen runs) — fps scales
#            ~1/cell-count, so compare numbers only at the same window
#            geometry. FIRE_FIXED=1 switches to the fixed 80x24 region
#            (byte-identical stream, geometry-independent).
#            LAT_ITERS (default 1000), LAT_WARMUP (default 100).
set -u

BENCH_DIR="$(cd "$(dirname "$0")" && pwd)"
TOOLS="$BENCH_DIR/tools/bin"

YES=0
TESTS=""
for a in "$@"; do
  case "$a" in
    --yes|-y) YES=1 ;;
    --list) echo "tests: ascii unicode scroll fire latency"; exit 0 ;;
    ascii|unicode|scroll|fire|latency) TESTS="$TESTS $a" ;;
    *) echo "unknown arg: $a (tests: ascii unicode scroll fire latency; flags: --yes, --list)" >&2; exit 2 ;;
  esac
done
[ -z "$TESTS" ] && TESTS="ascii unicode scroll fire latency"

if [ ! -t 1 ]; then
  echo "WARNING: stdout is not a tty — without pty flow control these numbers" >&2
  echo "measure the pipe/file, not a terminal. Run this inside the terminal" >&2
  echo "you want to measure." >&2
fi

selected() { case " $TESTS " in *" $1 "*) return 0 ;; *) return 1 ;; esac; }

confirm() {
  [ "$YES" = 1 ] && return 0
  read -r -p "Press [Enter] to start the $1 test (this floods the terminal)..."
}

# ── prerequisites (only for the selected tests) ─────────────────────
if { selected ascii || selected unicode; } && \
   { [ ! -f "$BENCH_DIR/150MB_ascii.txt" ] || [ ! -f "$BENCH_DIR/150MB_unicode.txt" ]; }; then
  echo "Generating throughput data files (150MB each)..."
  (cd "$BENCH_DIR" && python3 generate_data.py)
fi
if selected scroll && [ ! -f "$BENCH_DIR/scroll_stress.txt" ]; then
  echo "Generating scroll stress file..."
  (cd "$BENCH_DIR" && python3 gen_scroll.py 40 scroll_stress.txt)
fi
# nowns is needed by every test's timing; fire/latency need their own tool.
build_tool() { # name [extra cc flags...]
  local n="$1"; shift
  if [ ! -x "$TOOLS/$n" ] || [ "$BENCH_DIR/tools/$n.c" -nt "$TOOLS/$n" ]; then
    mkdir -p "$TOOLS"
    (cd "$BENCH_DIR/tools" && cc -O2 "$@" -o "bin/$n" "$n.c") || exit 1
  fi
}
build_tool nowns
selected fire && build_tool fire
selected latency && build_tool dsr_probe
NOWNS="$TOOLS/nowns"

SUMMARY=""
note() { SUMMARY="$SUMMARY  - $1\n"; }

# ── cat-workload tests (ascii / unicode / scroll) ───────────────────
run_cat_test() { # name file
  local name="$1" file="$2"
  local bytes; bytes=$(stat -f%z "$file")
  confirm "$name"
  echo "--- START: $name ---"
  local t0 t1
  t0=$("$NOWNS")
  cat "$file"
  t1=$("$NOWNS")
  # The workload files leave terminal state behind (scroll_stress sets a
  # DECSTBM scroll region and homes the cursor every 500 lines; the unicode
  # file embeds SGR test cases), so anything printed next would overlap the
  # flood residue mid-screen. Reset attributes + scroll region and park the
  # cursor on the last row so output continues scrolling normally.
  printf '\033[0m\033[r\033[9999;1H\n'
  echo "--- END: $name ---"
  local ms mibs
  ms=$(awk -v ns=$((t1 - t0)) 'BEGIN{printf "%.0f", ns/1e6}')
  mibs=$(awk -v b="$bytes" -v ns=$((t1 - t0)) 'BEGIN{printf "%.1f", (b/1048576)/(ns/1e9)}')
  echo "$name: ${ms} ms (${mibs} MiB/s)"
  note "$name: ${ms} ms (${mibs} MiB/s)"
}

selected ascii   && run_cat_test ascii   "$BENCH_DIR/150MB_ascii.txt"
selected unicode && run_cat_test unicode "$BENCH_DIR/150MB_unicode.txt"
selected scroll  && run_cat_test scroll  "$BENCH_DIR/scroll_stress.txt"

# ── fire (DOOM-fire IO stress) ──────────────────────────────────────
if selected fire; then
  FIRE_SECS="${FIRE_SECS:-10}"
  fire_mode_arg="full"
  fire_mode_desc="full window (upstream DOOM-fire condition; fps depends on window geometry)"
  if [ "${FIRE_FIXED:-0}" = 1 ]; then
    fire_mode_arg=""
    fire_mode_desc="fixed 80x24 region (byte-identical stream, geometry-independent)"
  fi
  confirm "fire (${FIRE_SECS}s, ${fire_mode_desc})"
  result=$(mktemp) || exit 1
  "$TOOLS/fire" "$FIRE_SECS" "$result" $fire_mode_arg
  read -r frames elapsed_ns fps winsz region < "$result"
  rm -f "$result"
  # Cell-normalized throughput: fps depends on region geometry (full mode
  # follows the live window), so Mcells/s = fps x region cells is the
  # cross-terminal-comparable number.
  mcells=$(awk -v fps="$fps" -v r="$region" \
    'BEGIN{n=split(r,a,"x"); if(n==2) printf "%.2f", fps*a[1]*a[2]/1e6; else printf "?"}')
  echo "fire: ${fps} fps / ${mcells} Mcells/s (${frames} frames / ${FIRE_SECS}s, region ${region}, winsize ${winsz})"
  note "fire: ${fps} fps / ${mcells} Mcells/s (${frames} frames, ${FIRE_SECS}s, region ${region})"
fi

# ── latency (DSR round-trip proxy) ──────────────────────────────────
if selected latency; then
  LAT_ITERS="${LAT_ITERS:-1000}"
  LAT_WARMUP="${LAT_WARMUP:-100}"
  confirm "latency (${LAT_ITERS} iterations)"
  result=$(mktemp) || exit 1
  "$TOOLS/dsr_probe" "$LAT_ITERS" "$LAT_WARMUP" "$result" ""
  read -r med p95 p99 max min count < "$result"
  rm -f "$result"
  if [ "${count:-0}" -gt 0 ] 2>/dev/null && [ "${med:-0}" -gt 0 ] 2>/dev/null; then
    lat_line=$(awk -v m="$med" -v a="$p95" -v b="$p99" -v x="$max" -v c="$count" \
      'BEGIN{printf "median %.1f / p95 %.1f / p99 %.1f / max %.1f µs (%d samples)", m/1000, a/1000, b/1000, x/1000, c}')
  else
    lat_line="UNMEASURED (no DSR reply — is this a real terminal?)"
  fi
  echo "latency: $lat_line"
  note "latency: $lat_line"
fi

# ── summary ─────────────────────────────────────────────────────────
echo
echo "========================================="
echo "           BENCHMARK SUMMARY"
echo "========================================="
printf "%b" "$SUMMARY"
echo "========================================="
echo "Scored cross-terminal comparison: bench/run_all.sh (see METHODOLOGY.md)."
