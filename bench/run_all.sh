#!/usr/bin/env bash
# run_all.sh — reproducible 4-axis terminal benchmark harness.
#
#   Axes: throughput (150MB ascii/unicode consume), input-latency (DSR proxy),
#         frame/scroll (SGR + scroll-region stress consume), warm startup.
#   Terminals: noa (target/release), Ghostty, Termy, kitty — whichever exist.
#
# One command runs the whole suite and writes a machine-readable results.json
# plus a human table.md into bench/results/<timestamp>/. See METHODOLOGY.md.
#
# Usage:
#   bench/run_all.sh                 # full suite, all present terminals
#   bench/run_all.sh --quick         # 1 rep / smaller data (smoke)
#   bench/run_all.sh --only noa,kitty
set -u

BENCH_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$BENCH_DIR/.." && pwd)"
TOOLS="$BENCH_DIR/tools/bin"
WRAPPER="$BENCH_DIR/wrapper.sh"
NOWNS="$TOOLS/nowns"
PROBE="$TOOLS/dsr_probe"

# ── options ────────────────────────────────────────────────────────
QUICK=0
ONLY=""
EQUALIZE=0
# equalized-condition targets (used only with --equalize)
EQ_COLS=120; EQ_ROWS=40; EQ_FONT="Menlo"; EQ_FSIZE=14
while [ $# -gt 0 ]; do
  case "$1" in
    --quick) QUICK=1 ;;
    --only) ONLY="$2"; shift ;;
    --equalize) EQUALIZE=1 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
  shift
done

TS="$(date +%Y%m%d-%H%M%S)"
OUT_DIR="$BENCH_DIR/results/$TS"
mkdir -p "$OUT_DIR"
RAW="$OUT_DIR/raw.tsv"
RUNTMP="$(mktemp -d)"
trap 'rm -rf "$RUNTMP"' EXIT
printf 'terminal\taxis\tvariant\trep\tmetric\tvalue\tunit\n' > "$RAW"

# reps / timeouts (seconds)
if [ "$QUICK" = 1 ]; then
  TP_REPS=1; SCROLL_REPS=1; LAT_RUNS=1; START_REPS=2
else
  TP_REPS=3; SCROLL_REPS=3; LAT_RUNS=2; START_REPS=5
fi
TP_TIMEOUT=180; SCROLL_TIMEOUT=120; LAT_TIMEOUT=60; START_TIMEOUT=30

# ── data files ─────────────────────────────────────────────────────
ASCII="$BENCH_DIR/150MB_ascii.txt"
UNICODE="$BENCH_DIR/150MB_unicode.txt"
SCROLLF="$BENCH_DIR/scroll_stress.txt"
if [ ! -f "$ASCII" ] || [ ! -f "$UNICODE" ]; then
  (cd "$BENCH_DIR" && python3 generate_data.py)
fi
[ -f "$SCROLLF" ] || (cd "$BENCH_DIR" && python3 gen_scroll.py 40 scroll_stress.txt)

# ── tools ──────────────────────────────────────────────────────────
[ -x "$NOWNS" ] && [ -x "$PROBE" ] || (cd "$BENCH_DIR/tools" && mkdir -p bin && \
  cc -O2 -o bin/nowns nowns.c && cc -O2 -o bin/dsr_probe dsr_probe.c)
chmod +x "$WRAPPER"

# ── equalize: swap user configs for minimal ones, restore on exit ──
NOA_CFG="$HOME/.config/noa/config"
TERMY_CFG="$HOME/.config/termy/config.txt"
restore_configs() {
  [ -f "$RUNTMP/noa.config.bak" ] && cp "$RUNTMP/noa.config.bak" "$NOA_CFG"
  [ -f "$RUNTMP/termy.config.bak" ] && cp "$RUNTMP/termy.config.bak" "$TERMY_CFG"
}
trap 'restore_configs; rm -rf "$RUNTMP"' EXIT
if [ "$EQUALIZE" = 1 ]; then
  echo "EQUALIZE: pinning ${EQ_COLS}x${EQ_ROWS}, font ${EQ_FONT} ${EQ_FSIZE}pt (no ligatures / bg effects)"
  if [ -f "$NOA_CFG" ]; then
    cp "$NOA_CFG" "$RUNTMP/noa.config.bak"
    # noa: --cols/--rows/--font-size come from CLI; font-family + stripping
    # ligatures/background effects can only come from the config file.
    printf 'font-family = %s\nfont-size = %s\nbackground-opacity = 1.00\nsidebar-enabled = false\n' \
      "$EQ_FONT" "$EQ_FSIZE" > "$NOA_CFG"
  fi
  if [ -f "$TERMY_CFG" ]; then
    cp "$TERMY_CFG" "$RUNTMP/termy.config.bak"
    # Termy has no CLI size/font control; font is settable via config only.
    # (Grid size has no config key -> stays at native default; documented.)
    { grep -vE '^\s*#?\s*(font_family|font_size)\s*=' "$RUNTMP/termy.config.bak"
      printf 'font_family = %s\nfont_size = %s\n' "$EQ_FONT" "$EQ_FSIZE"
    } > "$TERMY_CFG"
  fi
fi

# ── terminal registry ──────────────────────────────────────────────
NOA_BIN="$REPO_DIR/target/release/noa"
GHOSTTY_BIN="/Applications/Ghostty.app/Contents/MacOS/ghostty"
TERMY_BIN="/Applications/Termy.app/Contents/MacOS/termy"
KITTY_BIN="/Applications/kitty.app/Contents/MacOS/kitty"

term_present() {
  case "$1" in
    noa) [ -x "$NOA_BIN" ] ;;
    ghostty) [ -x "$GHOSTTY_BIN" ] ;;
    termy) [ -x "$TERMY_BIN" ] ;;
    kitty) [ -x "$KITTY_BIN" ] ;;
    *) return 1 ;;
  esac
}

# Launch a terminal (fresh process) running $WRAPPER as its pty child.
# Env (NOA_MODE etc.) is already exported by the caller and inherited.
launch_term() {
  if [ "$EQUALIZE" = 1 ]; then
    case "$1" in
      noa)     SHELL="$WRAPPER" "$NOA_BIN" --cols "$EQ_COLS" --rows "$EQ_ROWS" --font-size "$EQ_FSIZE" >/dev/null 2>&1 & ;;
      ghostty) "$GHOSTTY_BIN" --config-default-files=false --font-family="$EQ_FONT" --font-size="$EQ_FSIZE" \
                 --window-width="$EQ_COLS" --window-height="$EQ_ROWS" -e "$WRAPPER" >/dev/null 2>&1 & ;;
      termy)   SHELL="$WRAPPER" "$TERMY_BIN" >/dev/null 2>&1 & ;;  # size not controllable
      kitty)   "$KITTY_BIN" --config NONE -o remember_window_size=no \
                 -o initial_window_width="${EQ_COLS}c" -o initial_window_height="${EQ_ROWS}c" \
                 -o font_family="$EQ_FONT" -o font_size="$EQ_FSIZE" -o confirm_os_window_close=0 "$WRAPPER" >/dev/null 2>&1 & ;;
    esac
  else
    case "$1" in
      noa)     SHELL="$WRAPPER" "$NOA_BIN" --cols 120 --rows 40 >/dev/null 2>&1 & ;;
      ghostty) "$GHOSTTY_BIN" -e "$WRAPPER" >/dev/null 2>&1 & ;;
      termy)   SHELL="$WRAPPER" "$TERMY_BIN" >/dev/null 2>&1 & ;;
      kitty)   "$KITTY_BIN" -1=no -o confirm_os_window_close=0 "$WRAPPER" >/dev/null 2>&1 & ;;
    esac
  fi
  echo $!
}

kill_term() {
  case "$1" in
    noa)     pkill -f "$NOA_BIN" 2>/dev/null ;;  # full path: don't kill other worktrees' noa
    ghostty) pkill -x ghostty 2>/dev/null ;;
    termy)   pkill -x termy 2>/dev/null ;;
    kitty)   pkill -x kitty 2>/dev/null; pkill -x kitten 2>/dev/null ;;
  esac
  return 0
}

emit() { printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$4" "$5" "$6" "$7" >> "$RAW"; }

# Wait until $1 exists or $2 seconds elapse. Return 0 if appeared, 1 on timeout.
wait_sentinel() {
  local f="$1" timeout="$2" waited=0
  local deadline=$(( $(date +%s) + timeout ))
  while [ ! -f "$f" ]; do
    [ "$(date +%s)" -ge "$deadline" ] && return 1
    sleep 0.01
  done
  return 0
}

# ── one measured run ───────────────────────────────────────────────
# args: term mode timeout [workload-cmd]
# echoes on stdout, for throughput/scroll: "<inner_ns> <total_ns>"
#                    for startup:           "<total_ns>"
#                    for latency:           "<median_ns> <p99_ns> <min_ns> <count>"
# returns 1 on timeout.
run_once() {
  local term="$1" mode="$2" timeout="$3" cmd="${4:-}"
  local sentinel="$RUNTMP/${term}.${mode}.$RANDOM.sentinel"
  local result="$RUNTMP/${term}.${mode}.$RANDOM.result"
  rm -f "$sentinel" "$result"

  kill_term "$term"; sleep 0.4

  export NOA_MODE="$mode" NOA_SENTINEL="$sentinel" NOA_NOWNS="$NOWNS" \
         NOA_PROBE="$PROBE" NOA_RESULT="$result" NOA_BENCH_CMD="$cmd"

  local t0 t1
  t0="$("$NOWNS")"
  launch_term "$term" >/dev/null
  if ! wait_sentinel "$sentinel" "$timeout"; then
    kill_term "$term"; sleep 0.3
    return 1
  fi
  t1="$("$NOWNS")"
  kill_term "$term"; sleep 0.4

  case "$mode" in
    throughput|scroll)
      # sentinel holds "<start_ns> <end_ns>" from inside the pty child.
      read -r s e < "$sentinel"
      echo "$((e - s)) $((t1 - t0))"
      ;;
    startup)
      echo "$((t1 - t0))"
      ;;
    latency)
      cat "$result"
      ;;
  esac
  return 0
}

median() { sort -n | awk '{a[NR]=$1} END{ if(NR==0){print 0} else if(NR%2){print a[(NR+1)/2]} else {print int((a[NR/2]+a[NR/2+1])/2)} }'; }

# ── select terminals ───────────────────────────────────────────────
ALL="noa ghostty termy kitty"
SELECTED=""
for t in $ALL; do
  if [ -n "$ONLY" ]; then case ",$ONLY," in *",$t,"*) ;; *) continue ;; esac; fi
  if term_present "$t"; then SELECTED="$SELECTED $t"; else
    echo "skip (not installed): $t"; emit "$t" meta - - present 0 bool
  fi
done
echo "Terminals under test:$SELECTED"
echo "Results -> $OUT_DIR"

# ── THROUGHPUT ─────────────────────────────────────────────────────
for term in $SELECTED; do
  for variant in ascii unicode; do
    case "$variant" in ascii) file="$ASCII";; unicode) file="$UNICODE";; esac
    bytes=$(stat -f%z "$file")
    echo "[throughput/$variant] $term"
    inner_samples=""
    ok=1
    for r in $(seq 1 $TP_REPS); do
      out="$(run_once "$term" throughput "$TP_TIMEOUT" "cat '$file'")" || { ok=0; break; }
      inner_ns="${out%% *}"; total_ns="${out##* }"
      emit "$term" throughput "$variant" "$r" inner_ns "$inner_ns" ns
      emit "$term" throughput "$variant" "$r" total_ns "$total_ns" ns
      inner_samples="$inner_samples$inner_ns\n"
    done
    if [ "$ok" = 1 ]; then
      med=$(printf "$inner_samples" | median)
      mbps=$(awk -v b="$bytes" -v ns="$med" 'BEGIN{ if(ns>0) printf "%.1f", (b/1048576)/(ns/1e9); else print 0 }')
      emit "$term" throughput "$variant" median inner_ns "$med" ns
      emit "$term" throughput "$variant" median mib_per_s "$mbps" mib_s
      echo "    median ${mbps} MiB/s"
    else
      emit "$term" throughput "$variant" - status UNMEASURED timeout
      echo "    UNMEASURED (timeout)"
    fi
  done
done

# ── FRAME / SCROLL ─────────────────────────────────────────────────
sbytes=$(stat -f%z "$SCROLLF")
for term in $SELECTED; do
  echo "[scroll] $term"
  samples=""; ok=1
  for r in $(seq 1 $SCROLL_REPS); do
    out="$(run_once "$term" scroll "$SCROLL_TIMEOUT" "cat '$SCROLLF'")" || { ok=0; break; }
    inner_ns="${out%% *}"
    emit "$term" scroll - "$r" inner_ns "$inner_ns" ns
    samples="$samples$inner_ns\n"
  done
  if [ "$ok" = 1 ]; then
    med=$(printf "$samples" | median)
    mbps=$(awk -v b="$sbytes" -v ns="$med" 'BEGIN{ if(ns>0) printf "%.1f",(b/1048576)/(ns/1e9); else print 0 }')
    ms=$(awk -v ns="$med" 'BEGIN{printf "%.0f", ns/1e6}')
    emit "$term" scroll - median inner_ns "$med" ns
    emit "$term" scroll - median mib_per_s "$mbps" mib_s
    echo "    median ${ms} ms (${mbps} MiB/s)"
  else
    emit "$term" scroll - - status UNMEASURED timeout
    echo "    UNMEASURED (timeout)"
  fi
done

# Latency + startup are condition-independent; skip them under --equalize
# (the equalized re-run targets the render-sensitive throughput+scroll axes).
if [ "$EQUALIZE" != 1 ]; then
# ── INPUT LATENCY (DSR round-trip proxy) ───────────────────────────
for term in $SELECTED; do
  echo "[latency] $term"
  med_samples=""; p99_samples=""; got=0
  for r in $(seq 1 $LAT_RUNS); do
    out="$(run_once "$term" latency "$LAT_TIMEOUT")" || continue
    set -- $out
    med_ns="${1:-0}"; p99_ns="${2:-0}"; min_ns="${3:-0}"; cnt="${4:-0}"
    if [ "$cnt" -eq 0 ]; then continue; fi
    got=$((got + 1))
    emit "$term" latency - "$r" median_ns "$med_ns" ns
    emit "$term" latency - "$r" p99_ns "$p99_ns" ns
    emit "$term" latency - "$r" min_ns "$min_ns" ns
    med_samples="$med_samples$med_ns\n"; p99_samples="$p99_samples$p99_ns\n"
  done
  if [ "$got" -ge 1 ]; then
    med=$(printf "$med_samples" | median); p99=$(printf "$p99_samples" | median)
    emit "$term" latency - median median_ns "$med" ns
    emit "$term" latency - median p99_ns "$p99" ns
    mus=$(awk -v ns="$med" 'BEGIN{printf "%.1f", ns/1000}')
    echo "    median ${mus} us"
  else
    emit "$term" latency - - status UNMEASURED "no-dsr-reply-within-timeout-render-thread-gated"
    echo "    UNMEASURED (no DSR reply within timeout — reply appears render-thread/focus-gated)"
  fi
done

# ── WARM STARTUP ───────────────────────────────────────────────────
for term in $SELECTED; do
  echo "[startup] $term"
  # one warm-up (not recorded), then START_REPS recorded
  run_once "$term" startup "$START_TIMEOUT" >/dev/null 2>&1 || true
  samples=""; ok=1
  for r in $(seq 1 $START_REPS); do
    out="$(run_once "$term" startup "$START_TIMEOUT")" || { ok=0; break; }
    emit "$term" startup - "$r" total_ns "$out" ns
    samples="$samples$out\n"
  done
  if [ "$ok" = 1 ]; then
    med=$(printf "$samples" | median)
    ms=$(awk -v ns="$med" 'BEGIN{printf "%.0f", ns/1e6}')
    emit "$term" startup - median total_ns "$med" ns
    echo "    median ${ms} ms"
  else
    emit "$term" startup - - status UNMEASURED timeout
    echo "    UNMEASURED (timeout)"
  fi
done
fi  # end non-equalize axes

# record equalization status per terminal (for the report / methodology)
if [ "$EQUALIZE" = 1 ]; then
  for term in $SELECTED; do
    case "$term" in
      noa)     emit "$term" meta - - equalized "grid=${EQ_COLS}x${EQ_ROWS};font=${EQ_FONT}@${EQ_FSIZE};ligatures-off;bg-off" note ;;
      ghostty) emit "$term" meta - - equalized "grid=${EQ_COLS}x${EQ_ROWS};font=${EQ_FONT}@${EQ_FSIZE};clean-config" note ;;
      kitty)   emit "$term" meta - - equalized "grid=${EQ_COLS}x${EQ_ROWS};font=${EQ_FONT}@${EQ_FSIZE};config=NONE" note ;;
      termy)   emit "$term" meta - - equalized "grid=NATIVE-DEFAULT(no-size-key);font=${EQ_FONT}@${EQ_FSIZE}" note ;;
    esac
  done
fi

# ── final cleanup ──────────────────────────────────────────────────
for term in $SELECTED; do kill_term "$term"; done

# ── aggregate → json + markdown ────────────────────────────────────
python3 "$BENCH_DIR/aggregate.py" "$RAW" "$OUT_DIR" "$TS"
cp "$BENCH_DIR/METHODOLOGY.md" "$OUT_DIR/METHODOLOGY.md" 2>/dev/null || true
echo
echo "==================================================================="
cat "$OUT_DIR/table.md"
echo "==================================================================="
echo "JSON:  $OUT_DIR/results.json"
echo "Table: $OUT_DIR/table.md"
