#!/usr/bin/env bash
# run_all.sh — reproducible 6-axis terminal benchmark harness.
#
#   Axes: throughput (150MB ascii/unicode consume), input-latency (DSR proxy),
#         frame/scroll (SGR + scroll-region stress consume), warm startup,
#         memory (idle/scrollback/multitab/longevity footprint — every
#         scenario dual-reported as active @15s + settled @90s, ranked on
#         settled), load (idle CPU% + active CPU-time-per-workload).
#   Terminals: noa (target/release), Ghostty, Termy, kitty — whichever exist.
#
# One command runs the whole suite and writes a machine-readable results.json
# plus a human table.md into bench/results/<timestamp>/. See METHODOLOGY.md.
#
# COLLATERAL-SAFETY INVARIANT: the harness NEVER kills, samples, or activates
# a process by name or path. Every launched pid is recorded in a per-terminal
# registry ($RUNTMP/pids.<term>) and lifecycle/measurement operate strictly on
# those pids + their descendant trees. The user's own running instances of
# Ghostty/kitty/Termy/noa are never touched by a bench run.
#
# Usage:
#   bench/run_all.sh                 # full suite, all present terminals
#   bench/run_all.sh --quick         # 1 rep / smaller data (smoke)
#   bench/run_all.sh --only noa,kitty
#   bench/run_all.sh --axes latency,scroll   # subset of the six axes
#   bench/run_all.sh --force                 # skip the builder-quiescence gate
#   NOA_BIN=/path/to/noa bench/run_all.sh    # measure a non-default noa build
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
AXES="throughput,scroll,latency,startup,memory,load"
EQUALIZE=0
FORCE=0
# equalized-condition targets (used only with --equalize)
EQ_COLS=120; EQ_ROWS=40; EQ_FONT="Menlo"; EQ_FSIZE=14
while [ $# -gt 0 ]; do
  case "$1" in
    --quick) QUICK=1 ;;
    --only) ONLY="$2"; shift ;;
    --axes) AXES="$2"; shift ;;
    --equalize) EQUALIZE=1 ;;
    --force) FORCE=1 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
  shift
done

axis_selected() { case ",$AXES," in *",$1,"*) return 0 ;; *) return 1 ;; esac; }

TS="$(date +%Y%m%d-%H%M%S)"
OUT_DIR="$BENCH_DIR/results/$TS"
mkdir -p "$OUT_DIR"
RAW="$OUT_DIR/raw.tsv"
RUNTMP="$(mktemp -d)"
trap 'rm -rf "$RUNTMP"' EXIT
printf 'terminal\taxis\tvariant\trep\tmetric\tvalue\tunit\n' > "$RAW"

emit() { printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$4" "$5" "$6" "$7" >> "$RAW"; }

# ── quiescence gate + contention instrumentation ───────────────────
# The render axes are contention-sensitive (see METHODOLOGY.md "Contention
# sensitivity"): a concurrent cargo/rustc/clang build silently inflates
# CPU-bound terminals' numbers. So: (1) refuse to start while builders are
# alive unless --force (read-only `ps` scan — the gate never kills anything),
# and (2) record loadavg/uptime at run start AND end into results.json so a
# contaminated run is detectable after the fact.
builders_alive() {
  # pid:basename pairs of any live cargo/rustc/clang across the whole system
  ps -axo pid=,comm= 2>/dev/null | awk '{
    n = split($2, seg, "/"); c = seg[n]
    if (c == "cargo" || c == "rustc" || c == "clang" || c == "clang++")
      printf "%s:%s ", $1, c
  }'
}
BUILDERS="$(builders_alive)"
if [ -n "$BUILDERS" ] && [ "$FORCE" != 1 ]; then
  echo "REFUSING TO START: compiler/build processes are alive: $BUILDERS" >&2
  echo "The load/latency/scroll axes are contention-sensitive; results taken" >&2
  echo "now would be unattributable. Wait for the build(s) to finish, or" >&2
  echo "re-run with --force to record anyway (the gate result is logged)." >&2
  rm -rf "$OUT_DIR"
  exit 3
fi
if [ -n "$BUILDERS" ]; then
  emit harness meta - - quiescence_check "FORCED-PAST-LIVE-BUILDERS: $BUILDERS" note
  echo "WARNING (--force): builders alive at start: $BUILDERS"
else
  emit harness meta - - quiescence_check "clear (no cargo/rustc/clang alive at start)" note
fi
emit harness meta - - loadavg_start "$(sysctl -n vm.loadavg 2>/dev/null)" note
emit harness meta - - uptime_start "$(uptime)" note

# reps / timeouts (seconds)
if [ "$QUICK" = 1 ]; then
  TP_REPS=1; SCROLL_REPS=1; LAT_RUNS=2; START_REPS=2
  # latency (quick): 2 launches x 200 kept iterations — smoke only, the
  # pooled tail stats are indicative, not decisive.
  LAT_ITERS=200; LAT_WARMUP=20
  MEM_ACTIVE_AT_S=3; MEM_SAMPLE_EVERY_S=5; MEM_SETTLED_UNTIL_S=30
  MEM_MULTITAB_N=3; MEM_LONGEVITY_CYCLES=2; MEM_LONGEVITY_IDLE_S=1
  LOAD_IDLE_SETTLE_S=5; LOAD_IDLE_S=10
else
  TP_REPS=3; SCROLL_REPS=3; LAT_RUNS=10; START_REPS=5
  # latency (full): 10 process launches x 1000 kept iterations each (100
  # warmup discarded per launch, proportional to the old 200/20) = >=10000
  # kept samples pooled per terminal, so the reported p99 rests on ~100
  # tail-side samples instead of the ~2 a 200-iteration run gives it.
  LAT_ITERS=1000; LAT_WARMUP=100
  # memory: every scenario is DUAL-REPORTED (see METHODOLOGY.md "Memory"):
  #   active  = footprint at t=MEM_ACTIVE_AT_S ("in-use footprint")
  #   settled = median of the last 3 samples of a trajectory sampled every
  #             MEM_SAMPLE_EVERY_S up to t=MEM_SETTLED_UNTIL_S ("long-lived
  #             idle footprint", past the macOS GPU-driver-pool reclaim that
  #             lands nondeterministically 5-40s after the last GPU submit).
  # The full trajectory goes to raw.tsv so either number can be audited.
  MEM_ACTIVE_AT_S=15; MEM_SAMPLE_EVERY_S=5; MEM_SETTLED_UNTIL_S=90
  MEM_MULTITAB_N=10; MEM_LONGEVITY_CYCLES=5; MEM_LONGEVITY_IDLE_S=3
  # load-idle: discard the first LOAD_IDLE_SETTLE_S after launch (the dyld/
  # AppKit/first-frame transient decays through `ps pcpu`'s decaying average
  # for >10s), then sample the settled window for LOAD_IDLE_S.
  LOAD_IDLE_SETTLE_S=15; LOAD_IDLE_S=60
fi
TP_TIMEOUT=180; SCROLL_TIMEOUT=120; LAT_TIMEOUT=60; START_TIMEOUT=30
MEM_HOLD_TIMEOUT=20

# ── data files (only for the axes that consume them) ───────────────
ASCII="$BENCH_DIR/150MB_ascii.txt"
UNICODE="$BENCH_DIR/150MB_unicode.txt"
SCROLLF="$BENCH_DIR/scroll_stress.txt"
if { axis_selected throughput || axis_selected load; } && { [ ! -f "$ASCII" ] || [ ! -f "$UNICODE" ]; }; then
  (cd "$BENCH_DIR" && python3 generate_data.py)
fi
if axis_selected scroll || axis_selected memory || axis_selected load; then
  [ -f "$SCROLLF" ] || (cd "$BENCH_DIR" && python3 gen_scroll.py 40 scroll_stress.txt)
fi

# ── tools ──────────────────────────────────────────────────────────
# Rebuild when missing OR when any source is newer than its binary (a stale
# prebuilt dsr_probe would silently emit the old 4-field result format).
tools_fresh() {
  local t
  for t in nowns dsr_probe winwait wincount; do
    [ -x "$TOOLS/$t" ] || return 1
    [ "$BENCH_DIR/tools/$t.c" -nt "$TOOLS/$t" ] && return 1
  done
  return 0
}
tools_fresh || \
  (cd "$BENCH_DIR/tools" && mkdir -p bin && \
  cc -O2 -o bin/nowns nowns.c && cc -O2 -o bin/dsr_probe dsr_probe.c && \
  cc -O2 -framework ApplicationServices -o bin/winwait winwait.c && \
  cc -O2 -framework ApplicationServices -o bin/wincount wincount.c)
chmod +x "$WRAPPER"

# ── config isolation: every terminal runs FRESH-INSTALL DEFAULTS ───
# The scored comparison is fresh defaults for everyone (METHODOLOGY.md
# "Config isolation"). The user's real config files are NEVER read, written,
# or moved. Mechanism per terminal (each verified — see METHODOLOGY.md):
#   noa    XDG_CONFIG_HOME -> $ISO_XDG (noa-config honors it; the iso config
#          carries only `window-save-state = never`, which keeps harness runs
#          from overwriting the user's real session.json in ~/Library/
#          Application Support/noa — a data-dir path env can't redirect —
#          and is measurement-neutral: restore was already skipped because
#          the harness always passes --cols/--rows)
#   ghostty --config-default-files=false (reads NO config files at all; its
#          ~/Library/Application Support path would bypass an XDG redirect)
#          + --window-save-state=never (kills the phantom saved-state window,
#          see the launch block below)
#   termy  XDG_CONFIG_HOME -> $ISO_XDG (verified: creates a fresh default
#          config.txt inside the iso dir on first launch)
#   kitty  --config NONE (pure built-in defaults)
ISO_XDG="$RUNTMP/xdg-config"
mkdir -p "$ISO_XDG/noa" "$ISO_XDG/termy"
printf 'window-save-state = never\n' > "$ISO_XDG/noa/config"
# kill_all_tracked is defined below (needs the PID registry); `|| true` keeps
# an early exit (before definitions) from failing the trap.
trap 'kill_all_tracked 2>/dev/null || true; rm -rf "$RUNTMP"' EXIT
if [ "$EQUALIZE" = 1 ]; then
  echo "EQUALIZE: pinning ${EQ_COLS}x${EQ_ROWS}, font ${EQ_FONT} ${EQ_FSIZE}pt (no ligatures / bg effects)"
  # Equalized noa/termy settings are written into the ISOLATED config dir —
  # the user's real configs are never touched (pre-2026-07-16 harnesses
  # swapped the real files with backup/restore; the isolation dir removes
  # that whole risk class). ghostty/kitty get their equalized settings via
  # CLI flags in launch_term.
  printf 'font-family = %s\nfont-size = %s\nbackground-opacity = 1.00\nsidebar-enabled = false\nwindow-save-state = never\n' \
    "$EQ_FONT" "$EQ_FSIZE" > "$ISO_XDG/noa/config"
  # Termy has no CLI size/font control; font is settable via config only.
  # (Grid size has no config key -> stays at native default; documented.)
  printf 'font_family = %s\nfont_size = %s\n' "$EQ_FONT" "$EQ_FSIZE" > "$ISO_XDG/termy/config.txt"
fi

# ── terminal registry ──────────────────────────────────────────────
# NOA_BIN is overridable from the environment so a candidate build (e.g. a
# worktree's target/release/noa) can be measured against the same harness.
NOA_BIN="${NOA_BIN:-$REPO_DIR/target/release/noa}"
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

# ── PID-scoped process lifecycle ───────────────────────────────────
# Every pid the harness spawns is recorded in $RUNTMP/pids.<term>. All
# lifecycle (kill), measurement (app_pids), and activation operate ONLY on
# registered pids + their descendant trees — never on name/path matches, so a
# user's own running instance of the same terminal can never be collateral.

register_pid() { echo "$2" >> "$RUNTMP/pids.$1"; }

# proc_alive <pid> — alive and not a zombie (a SIGTERM'd direct child lingers
# as a zombie until the shell reaps it; treating it as alive would make every
# kill wait out the full grace period).
proc_alive() {
  local st; st="$(ps -o state= -p "$1" 2>/dev/null | tr -d ' ')"
  [ -n "$st" ] || return 1
  case "$st" in Z*) return 1 ;; esac
  return 0
}

# tracked_roots <term> — registered pids that are still alive AND still run
# the binary we launched (guards against pid reuse between kill cycles).
tracked_roots() {
  local term="$1" f="$RUNTMP/pids.$term" p comm want out=""
  [ -f "$f" ] || { echo ""; return; }
  case "$term" in
    noa) want="$NOA_BIN" ;;
    ghostty) want="$GHOSTTY_BIN" ;;
    termy) want="$TERMY_BIN" ;;
    kitty) want="$KITTY_BIN" ;;
    *) want="" ;;
  esac
  while read -r p; do
    proc_alive "$p" || continue
    comm="$(ps -o comm= -p "$p" 2>/dev/null)"
    # accept exact path or basename match (some apps re-exec with argv tweaks)
    case "$comm" in
      "$want"|*"/$(basename "$want")"|"$(basename "$want")") out="$out $p" ;;
    esac
  done < "$f"
  echo "$out"
}

# noa gained a Ghostty-style `-e <command...>` flag (2026-07). Detect it once
# so the harness can drive noa exactly like Ghostty (`-e $WRAPPER`) while
# older builds keep working through the $SHELL fallback.
if "$NOA_BIN" --help 2>/dev/null | grep -q '^  -e '; then NOA_HAS_E=1; else NOA_HAS_E=0; fi
# Config parity: ghostty runs --config-default-files=false and kitty runs
# --config NONE, so noa must not load the developer's personal config either
# (a user config with e.g. a rotating background-image, server-enable, or a
# remote client materially changes idle memory/CPU — the v3 "t=60s +116MB
# idle spike" was background-image-interval=60 firing a wallpaper rotation).
# Isolation is via XDG_CONFIG_HOME -> $ISO_XDG (verified honored via
# +show-config): the iso config carries ONLY the measurement-neutral
# `window-save-state = never` pin, so noa still runs fresh defaults while the
# user's real session.json stays untouched. `noa --config-default-files=false`
# exists for manual parity runs, but the harness prefers the XDG dir because
# the flag would also skip the iso pin file.

# Launch noa with the wrapper as pty child, preferring native `-e`.
# XDG_CONFIG_HOME -> $ISO_XDG = fresh-default config isolation (see above).
launch_noa() { # extra flags in "$@"
  if [ "$NOA_HAS_E" = 1 ]; then
    XDG_CONFIG_HOME="$ISO_XDG" "$NOA_BIN" "$@" -e "$WRAPPER" >/dev/null 2>&1 &
  else
    XDG_CONFIG_HOME="$ISO_XDG" SHELL="$WRAPPER" "$NOA_BIN" "$@" >/dev/null 2>&1 &
  fi
}

# Launch a terminal (fresh process) running $WRAPPER as its pty child.
# Env (NOA_MODE etc.) is already exported by the caller and inherited.
# Ghostty always gets --window-save-state=never: without it, macOS state
# restoration opens a SECOND surface per window running `login -> zsh`
# (the user's interactive shell, rc files and all) alongside the `-e`
# command — a phantom process tree that inflated every Ghostty memory/load
# number before 2026-07-16 (see METHODOLOGY.md "Ghostty phantom
# saved-state window"). kitty's confirm_os_window_close=0 is the one
# non-default setting the harness needs (kills must not hang on a confirm
# dialog); it is disclosed in METHODOLOGY.md.
launch_term() {
  if [ "$EQUALIZE" = 1 ]; then
    case "$1" in
      noa)     launch_noa --cols "$EQ_COLS" --rows "$EQ_ROWS" --font-size "$EQ_FSIZE" ;;
      ghostty) "$GHOSTTY_BIN" --config-default-files=false --window-save-state=never \
                 --font-family="$EQ_FONT" --font-size="$EQ_FSIZE" \
                 --window-width="$EQ_COLS" --window-height="$EQ_ROWS" -e "$WRAPPER" >/dev/null 2>&1 & ;;
      termy)   XDG_CONFIG_HOME="$ISO_XDG" SHELL="$WRAPPER" "$TERMY_BIN" >/dev/null 2>&1 & ;;  # size not controllable
      kitty)   "$KITTY_BIN" --config NONE -o remember_window_size=no \
                 -o initial_window_width="${EQ_COLS}c" -o initial_window_height="${EQ_ROWS}c" \
                 -o font_family="$EQ_FONT" -o font_size="$EQ_FSIZE" -o confirm_os_window_close=0 "$WRAPPER" >/dev/null 2>&1 & ;;
    esac
  else
    case "$1" in
      noa)     launch_noa --cols 120 --rows 40 ;;
      ghostty) "$GHOSTTY_BIN" --config-default-files=false --window-save-state=never -e "$WRAPPER" >/dev/null 2>&1 & ;;
      termy)   XDG_CONFIG_HOME="$ISO_XDG" SHELL="$WRAPPER" "$TERMY_BIN" >/dev/null 2>&1 & ;;
      kitty)   "$KITTY_BIN" --config NONE -o confirm_os_window_close=0 "$WRAPPER" >/dev/null 2>&1 & ;;
    esac
  fi
  register_pid "$1" "$!"
  echo $!
}

# Bring a just-launched terminal's window to the foreground. Some terminals
# (Termy; kitty is also sluggish unfocused) only answer DSR from their render
# path and only while focused, so the latency probe needs the window frontmost
# to measure them at all. PID-scoped: targets OUR tracked instance via System
# Events `unix id` so a user's own instance of the same app is never focused
# by mistake. Falls back to `open -a <bundle>` (focus-only, never kills) only
# if System Events automation is unavailable AND our instance is still alive —
# `open -a` on an already-dead instance would launch a fresh one.
activate_term() {
  local pid; pid="$(tracked_roots "$1")"; pid="$(echo $pid | awk '{print $1}')"
  [ -z "$pid" ] && return 0
  if osascript -e "tell application \"System Events\" to set frontmost of (first process whose unix id is $pid) to true" >/dev/null 2>&1; then
    return 0
  fi
  proc_alive "$pid" || return 0
  case "$1" in
    ghostty) open -a Ghostty 2>/dev/null || true ;;
    termy)   open -a Termy 2>/dev/null || true ;;
    kitty)   open -a kitty 2>/dev/null || true ;;
  esac
}

# kill_term <term> — kill ONLY the pids this harness spawned (and their
# descendant trees). SIGTERM first; after a short grace, SIGKILL survivors —
# a terminal with a live pty child (NOA_HOLD's `sleep`) may sit on a
# confirm-close dialog instead of exiting on SIGTERM, and a half-dead stale
# instance both pollutes the next measurement and can block the next launch.
kill_term() {
  local term="$1" f="$RUNTMP/pids.$term" roots tree p i alive
  [ -f "$f" ] || return 0
  roots="$(tracked_roots "$term")"
  if [ -n "$roots" ]; then
    # enumerate the tree BEFORE killing the roots (children reparent to
    # launchd once the root dies and would otherwise be lost as orphans,
    # e.g. the wrapper's `sleep 86400` hold child)
    tree="$(pid_tree_multi $roots)"
    # shellcheck disable=SC2086
    kill -TERM $tree 2>/dev/null
    for i in 1 2 3 4 5 6 7 8 9 10; do
      alive=""
      for p in $tree; do proc_alive "$p" && alive="$alive $p"; done
      [ -z "$alive" ] && break
      sleep 0.2
    done
    for p in $tree; do proc_alive "$p" && kill -KILL "$p" 2>/dev/null; done
  fi
  : > "$f"
  return 0
}

# kill_all_tracked — EXIT-trap safety net: kill every tracked pid of every
# terminal (still strictly PID-scoped), so an aborted run leaves nothing behind.
kill_all_tracked() {
  local f
  for f in "$RUNTMP"/pids.*; do
    [ -f "$f" ] || continue
    kill_term "${f##*/pids.}"
  done
  return 0
}

# ── memory/load helpers ─────────────────────────────────────────────
# Fairness note (see METHODOLOGY.md "memory/load axes"): "belongs to the app"
# is resolved from the PID REGISTRY (the pids this harness launched), expanded
# to the FULL descendant tree of every registered root. This covers both
# multi-process architectures seen here (helper processes per window/tab as
# with Ghostty/kitty, and plain one-process-per-window as with noa/Termy),
# while guaranteeing a user's own concurrently-running instance of the same
# terminal is never counted into the measurement (a name-based pgrep match
# would sum the user's processes into RSS/CPU). The pty child (wrapper.sh /
# its `cat`) is included as a descendant — it is the *same* tiny script for
# every terminal, so it adds an equal, negligible constant everywhere.

# pid_tree_multi <pid> [pid...] -> space-separated pid list: the given roots
# plus every transitive descendant, found by repeated ps -axo pid,ppid scans.
pid_tree_multi() {
  local all="" frontier="" p
  for p in "$@"; do
    case " $all " in *" $p "*) ;; *) all="$all $p"; frontier="$frontier $p" ;; esac
  done
  while [ -n "$frontier" ]; do
    local snapshot children new=""
    snapshot="$(ps -axo pid=,ppid= 2>/dev/null)"
    children="$(printf '%s\n' "$snapshot" | awk -v ppids="$frontier" \
      'BEGIN{n=split(ppids,arr," "); for(i=1;i<=n;i++) p[arr[i]]=1} p[$2]{print $1}')"
    for p in $children; do
      case " $all " in *" $p "*) ;; *) all="$all $p"; new="$new $p" ;; esac
    done
    frontier="$new"
  done
  printf '%s\n' "$all" | tr ' ' '\n' | grep -v '^$' | sort -un | tr '\n' ' '
}

# app_pids <term> -> full process tree belonging to that terminal: the pids
# THIS harness launched (tracked_roots) expanded via pid_tree_multi. Never a
# name/path match — see the PID-scoped lifecycle block above.
app_pids() {
  local roots; roots="$(tracked_roots "$1")"
  [ -z "$roots" ] && { echo ""; return; }
  pid_tree_multi $roots
}

# mem_footprint_bytes <pid list> -> summed macOS "physical footprint" (the
# same metric Activity Monitor's Memory column reports: dirty + compressed +
# IOSurface/GPU-owned, NOT plain RSS — these are wgpu/Metal apps, and RSS
# alone misses most of their real GPU-backed footprint). Uses the system
# `footprint` tool's JSON output so multi-process apps are summed exactly.
mem_footprint_bytes() {
  local pids="$1"
  [ -z "$pids" ] && { echo 0; return; }
  local j="$RUNTMP/footprint.$RANDOM.$RANDOM.json"
  # shellcheck disable=SC2086
  footprint -j "$j" -f bytes $pids >/dev/null 2>&1
  if [ -f "$j" ]; then
    python3 -c "
import json, sys
try:
    d = json.load(open(sys.argv[1]))
    print(sum(p.get('footprint', 0) for p in d.get('processes', [])))
except Exception:
    print(0)
" "$j"
    rm -f "$j"
  else
    echo 0
  fi
}

# cpu_time_cs <pid list> -> summed cumulative CPU time (user+sys, in
# centiseconds) across the given pids, parsed from `ps -o time=`
# ("[[HH:]MM:]SS.cc"). Per-pid so one dead pid can't fail the whole batch.
cpu_time_cs() {
  local pids="$1" total=0 p t v
  for p in $pids; do
    t="$(ps -o time= -p "$p" 2>/dev/null | tr -d ' ')"
    [ -z "$t" ] && continue
    v="$(printf '%s' "$t" | awk -F'[:.]' '{
      n=NF; cs=$n; s=$(n-1)
      m=(n>=3)?$(n-2):0; h=(n>=4)?$(n-3):0
      print h*360000 + m*6000 + s*100 + cs
    }')"
    total=$((total + v))
  done
  echo "$total"
}

# sum_pcpu <pid list> -> summed `ps -o pcpu=` (macOS's decaying-average CPU%,
# same figure Activity Monitor shows) across the given pids, one at a time so
# a pid that died between snapshots doesn't abort the whole query.
sum_pcpu() {
  local pids="$1" total=0 p v
  for p in $pids; do
    v="$(ps -o pcpu= -p "$p" 2>/dev/null | tr -d ' ')"
    case "$v" in ''|*[!0-9.]*) continue ;; esac
    total="$(awk -v a="$total" -v b="$v" 'BEGIN{printf "%.4f", a+b}')"
  done
  echo "$total"
}

# sum_csw <pid list> -> summed CUMULATIVE context switches across the given
# pids, read from `top -l 1 -stats pid,csw` (one full-process snapshot,
# filtered to our pids). Used as a WAKEUPS PROXY: this macOS build's `top`
# does not expose a `wakeups` -stats key (verified empirically — it errors),
# and `powermetrics` needs sudo (never prompted per policy). A mostly-asleep
# process context-switches when it wakes, so idle csw/s tracks idle wakeup
# rate closely; documented in METHODOLOGY.md.
sum_csw() {
  local pids="$1"
  [ -z "$pids" ] && { echo 0; return; }
  top -l 1 -stats pid,csw 2>/dev/null | awk -v pids="$pids" '
    BEGIN { n = split(pids, a, " "); for (i = 1; i <= n; i++) want[a[i]] = 1; total = 0 }
    ($1 + 0) > 0 && want[$1 + 0] { v = $2; gsub(/[^0-9]/, "", v); total += v + 0 }
    END { print total }'
}

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
#                    for latency: "<median_ns> <p95_ns> <p99_ns> <max_ns> <min_ns> <count>"
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
  if [ "$mode" = latency ]; then
    # Focus the fresh window so render-thread/focus-gated DSR responders
    # (Termy) reply. Backgrounded with a delay so the launch timestamp path
    # is untouched; the probe's blocking read simply resumes once focus
    # lands. Repeated: app registration with the window server can lag the
    # first attempt on a cold start. The delays are RANDOMIZED (+-0.7s
    # around the old fixed 1.0s/1.5s schedule): a deterministic schedule
    # phase-locks the activation (and whatever redraw it triggers) to the
    # same probe iterations on every run, which can systematically create
    # or mask tail collisions with the app's periodic timers.
    local jit1 jit2
    jit1="$(awk -v r="$RANDOM" 'BEGIN{printf "%.2f", 0.3 + (r % 141) / 100.0}')"
    jit2="$(awk -v r="$RANDOM" 'BEGIN{printf "%.2f", 0.8 + (r % 141) / 100.0}')"
    ( sleep "$jit1"; activate_term "$term"; sleep "$jit2"; activate_term "$term" ) >/dev/null 2>&1 &
  fi
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

# pooled_stats <file-of-ns-samples> -> "median p95 p99 max count" over the
# POOLED distribution (all kept iterations of all launches concatenated).
# Nearest-rank percentiles matching dsr_probe's own convention
# (idx = floor(n*q), 0-based), so a per-run number and the pooled number are
# directly comparable.
pooled_stats() {
  sort -n "$1" 2>/dev/null | awk '
    {a[NR]=$1}
    END{
      n=NR
      if(n==0){print "0 0 0 0 0"; exit}
      med=(n%2) ? a[(n+1)/2] : int((a[n/2]+a[n/2+1])/2)
      i95=int(n*0.95)+1; if(i95>n)i95=n
      i99=int(n*0.99)+1; if(i99>n)i99=n
      print med, a[i95], a[i99], a[n], n
    }'
}

# ── memory/load scenario runners ────────────────────────────────────
# All of these launch with NOA_HOLD=1 (see wrapper.sh): after the mode's own
# work the pty child sleeps instead of exiting, so terminals that close their
# window when the pty child exits stay alive long enough to be sampled.

# mem_dual_sample <term> <scenario> [rep-prefix] -> prints "<active> <settled>".
# The dual-metric sampler behind every memory scenario (METHODOLOGY.md
# "Memory: two numbers per scenario"): instead of one blind sleep — which
# lands randomly before/after the macOS GPU-driver-pool reclaim (5-40s after
# the last submit, driver-scheduled, ±10-80MB per process) — it samples the
# harness-launched process tree's footprint on a fixed clock:
#   t=MEM_ACTIVE_AT_S            -> "active"  (in-use footprint)
#   then every MEM_SAMPLE_EVERY_S until t>=MEM_SETTLED_UNTIL_S
#   settled = median of the last 3 trajectory samples (long-lived idle
#             footprint, past the reclaim window)
# Every sample is emitted to raw.tsv (rep "<prefix>t<sec>s") so both numbers
# and the reclaim step between them stay auditable. Caller owns lifecycle:
# the tree must already be launched+ready, and is left running on return.
mem_dual_sample() {
  local term="$1" scenario="$2" prefix="${3:-}"
  local t="$MEM_ACTIVE_AT_S" v active samples=""
  sleep "$MEM_ACTIVE_AT_S"
  active="$(mem_footprint_bytes "$(app_pids "$term")")"
  emit "$term" memory "$scenario" "${prefix}t${t}s" footprint_bytes "$active" bytes
  while [ "$t" -lt "$MEM_SETTLED_UNTIL_S" ]; do
    sleep "$MEM_SAMPLE_EVERY_S"
    t=$((t + MEM_SAMPLE_EVERY_S))
    v="$(mem_footprint_bytes "$(app_pids "$term")")"
    emit "$term" memory "$scenario" "${prefix}t${t}s" footprint_bytes "$v" bytes
    samples="$samples$v\n"
  done
  local settled; settled="$(printf "$samples" | tail -3 | median)"
  echo "$active $settled"
}

# run_mem_idle <term> -> prints "<active_bytes> <settled_bytes>", or empty on
# timeout. Trajectory rows land in raw.tsv (see mem_dual_sample).
run_mem_idle() {
  local term="$1" sentinel="$RUNTMP/${term}.memidle.sentinel"
  kill_term "$term"; sleep 0.4
  rm -f "$sentinel"
  export NOA_MODE=hold NOA_SENTINEL="$sentinel" NOA_HOLD=1 NOA_NOWNS="$NOWNS" NOA_BENCH_CMD=""
  launch_term "$term" >/dev/null
  if ! wait_sentinel "$sentinel" "$MEM_HOLD_TIMEOUT"; then kill_term "$term"; sleep 0.3; echo ""; return; fi
  mem_dual_sample "$term" idle
  kill_term "$term"; sleep 0.4
}

# run_mem_scrollback <term> -> "<active_bytes> <settled_bytes>" after
# consuming SCROLLF; same dual sampling clock, starting when the flood ends.
run_mem_scrollback() {
  local term="$1" sentinel="$RUNTMP/${term}.memscroll.sentinel"
  kill_term "$term"; sleep 0.4
  rm -f "$sentinel"
  export NOA_MODE=scroll NOA_SENTINEL="$sentinel" NOA_HOLD=1 NOA_NOWNS="$NOWNS" NOA_BENCH_CMD="cat '$SCROLLF'"
  launch_term "$term" >/dev/null
  if ! wait_sentinel "$sentinel" "$SCROLL_TIMEOUT"; then kill_term "$term"; sleep 0.3; echo ""; return; fi
  mem_dual_sample "$term" scrollback
  kill_term "$term"; sleep 0.4
}

# run_mem_multitab <term> -> prints "<active_bytes> <settled_bytes>
# <processes_observed> <windows_observed> <breakdown>". Opens MEM_MULTITAB_N
# windows by relaunching
# the terminal that many times (staggered). Fairness instrumentation:
#   windows_observed — on-screen layer-0 windows owned by OUR pids (wincount),
#     verifying every terminal actually materialized the same window count.
#   breakdown — "comm=count,..." composition of the process tree (also written
#     to $OUT_DIR/multitab_procs.<term>.txt with pid/ppid detail), so a
#     process-count asymmetry (e.g. per-window helper processes) is visible
#     and attributable instead of looking like a rigged comparison.
# If a terminal reuses a single process/window across launches (singleton
# behavior) that's real data, not failure — reported as-is, never faked.
run_mem_multitab() {
  local term="$1" n="$MEM_MULTITAB_N" i
  kill_term "$term"; sleep 0.4
  for i in $(seq 1 "$n"); do
    local sentinel="$RUNTMP/${term}.multitab.$i.sentinel"
    rm -f "$sentinel"
    export NOA_MODE=hold NOA_SENTINEL="$sentinel" NOA_HOLD=1 NOA_NOWNS="$NOWNS" NOA_BENCH_CMD=""
    launch_term "$term" >/dev/null
    sleep 0.3
  done
  # Dual sampling with ALL windows kept open through the whole trajectory
  # (active at t=MEM_ACTIVE_AT_S, settled at t=MEM_SETTLED_UNTIL_S).
  local dual; dual="$(mem_dual_sample "$term" multitab)"
  local pids; pids="$(app_pids "$term")"
  local nproc; nproc="$(printf '%s\n' $pids | wc -w | tr -d ' ')"
  # shellcheck disable=SC2086
  local nwin; nwin="$("$TOOLS/wincount" $pids 2>/dev/null || echo 0)"
  local pidcsv; pidcsv="$(echo $pids | tr ' ' ',')"
  ps -o pid=,ppid=,comm= -p "$pidcsv" > "$OUT_DIR/multitab_procs.$term.txt" 2>/dev/null
  # comm basename counts; spaces flattened so the result stays one field
  local breakdown; breakdown="$(awk '{ n = split($NF, seg, "/"); c[seg[n]]++ }
    END { for (k in c) printf "%s=%d,", k, c[k] }' \
    "$OUT_DIR/multitab_procs.$term.txt" | sed 's/,$//' | tr ' ' '_')"
  kill_term "$term"; sleep 0.4
  echo "$dual $nproc $nwin ${breakdown:-unknown}"
}

# run_mem_longevity <term> -> line 1: one footprint-bytes sample per completed
# flood+idle cycle (space-separated), sampled from the SAME long-lived
# process across all cycles (a relaunch-per-cycle design would measure N cold
# starts, not longevity growth); line 2: "final <active> <settled>" — a full
# dual-sample trajectory of quiescence AFTER the last cycle (raw.tsv reps
# "final_t<sec>s"). The short inter-cycle idle (MEM_LONGEVITY_IDLE_S) is the
# "under churn" story and is deliberately kept short; the final settled
# reading answers "where does it sit once churn stops?" past GPU-pool reclaim.
# Line 2 only appears if every cycle completed.
run_mem_longevity() {
  local term="$1" cycles="$MEM_LONGEVITY_CYCLES" idle="$MEM_LONGEVITY_IDLE_S" i
  local sentinel="$RUNTMP/${term}.longevity.sentinel"
  kill_term "$term"; sleep 0.4
  rm -f "$sentinel"
  for i in $(seq 1 "$cycles"); do rm -f "$sentinel.cycle$i"; done
  # NOA_HOLD=1 matters most for the LAST cycle here: the wrapper's own loop
  # already sleeps $idle before every per-cycle sentinel, but nothing keeps
  # it alive after the final cycle's sentinel + the closing "$NOA_SENTINEL"
  # write, so without HOLD the process can exit before that last sample.
  export NOA_MODE=longevity NOA_SENTINEL="$sentinel" NOA_CYCLES="$cycles" NOA_IDLE_S="$idle" \
         NOA_HOLD=1 NOA_NOWNS="$NOWNS" NOA_BENCH_CMD="cat '$SCROLLF'"
  launch_term "$term" >/dev/null
  local samples="" done_cycles=0 i
  for i in $(seq 1 "$cycles"); do
    if ! wait_sentinel "$sentinel.cycle$i" "$SCROLL_TIMEOUT"; then break; fi
    samples="$samples$(mem_footprint_bytes "$(app_pids "$term")") "
    done_cycles=$((done_cycles + 1))
  done
  echo "$samples"
  if [ "$done_cycles" -eq "$cycles" ]; then
    echo "final $(mem_dual_sample "$term" longevity final_)"
  fi
  kill_term "$term"; sleep 0.4
}

# run_load_idle_sample <term> -> prints "<mean_pct> <max_pct> <csw_per_s>" of
# the SETTLED idle window: launch, discard the first LOAD_IDLE_SETTLE_S
# entirely (the dyld/AppKit/first-frame launch transient decays through
# `ps pcpu`'s decaying average for many seconds — the old 2s skip let it leak
# into mean AND max), then sample summed process-tree CPU% at ~1Hz for
# LOAD_IDLE_S seconds. csw_per_s is the context-switch delta across the same
# settled window divided by its measured duration (wakeups proxy, see
# sum_csw). Applied uniformly to every terminal.
run_load_idle_sample() {
  local term="$1" dur="$LOAD_IDLE_S" sentinel="$RUNTMP/${term}.loadidle.sentinel"
  kill_term "$term"; sleep 0.4
  rm -f "$sentinel"
  export NOA_MODE=hold NOA_SENTINEL="$sentinel" NOA_HOLD=1 NOA_NOWNS="$NOWNS" NOA_BENCH_CMD=""
  launch_term "$term" >/dev/null
  if ! wait_sentinel "$sentinel" "$MEM_HOLD_TIMEOUT"; then kill_term "$term"; sleep 0.3; echo ""; return; fi
  sleep "$LOAD_IDLE_SETTLE_S"   # settle: launch transient fully discarded
  local pids; pids="$(app_pids "$term")"
  local csw0 t0 csw1 t1
  csw0="$(sum_csw "$pids")"; t0="$("$NOWNS")"
  local sum=0 max=0 n=0 v gt i
  for i in $(seq 1 "$dur"); do
    v="$(sum_pcpu "$pids")"
    sum="$(awk -v a="$sum" -v b="$v" 'BEGIN{printf "%.4f", a+b}')"
    gt="$(awk -v b="$v" -v m="$max" 'BEGIN{print (b>m)?1:0}')"
    [ "$gt" = 1 ] && max="$v"
    n=$((n + 1))
    sleep 1
  done
  csw1="$(sum_csw "$pids")"; t1="$("$NOWNS")"
  kill_term "$term"; sleep 0.4
  local mean; mean="$(awk -v s="$sum" -v n="$n" 'BEGIN{ if(n>0) printf "%.2f", s/n; else print 0 }')"
  local csw_rate; csw_rate="$(awk -v a="$csw0" -v b="$csw1" -v t0="$t0" -v t1="$t1" \
    'BEGIN{ s=(t1-t0)/1e9; if(s>0 && b>=a) printf "%.1f", (b-a)/s; else print "0" }')"
  echo "$mean $max $csw_rate"
}

# run_load_active <term> <mode: throughput|scroll> <cmd> <timeout> -> prints
# process-tree CPU-time delta (ms, user+sys) consumed while <cmd> runs.
# Snapshots are taken on the terminal's own pid(s) right after they appear
# (before) and again right after workload completion (after); the pty child's
# `cat` itself is excluded by construction (it doesn't exist yet at "before"
# and has already exited/been reaped by "after") — deliberately, since `cat`
# is identical software run identically by every terminal here and isn't part
# of the terminal's own efficiency.
run_load_active() {
  local term="$1" mode="$2" cmd="$3" timeout="$4"
  local sentinel="$RUNTMP/${term}.loadactive.${mode}.sentinel"
  kill_term "$term"; sleep 0.4
  rm -f "$sentinel"
  export NOA_MODE="$mode" NOA_SENTINEL="$sentinel" NOA_HOLD=1 NOA_NOWNS="$NOWNS" NOA_BENCH_CMD="$cmd"
  launch_term "$term" >/dev/null
  local pids="" waited=0
  while [ -z "$pids" ] && [ "$waited" -lt 50 ]; do
    pids="$(app_pids "$term")"
    [ -z "$pids" ] && sleep 0.1
    waited=$((waited + 1))
  done
  local before; before="$(cpu_time_cs "$pids")"
  if ! wait_sentinel "$sentinel" "$timeout"; then kill_term "$term"; sleep 0.3; echo ""; return; fi
  local after; after="$(cpu_time_cs "$(app_pids "$term")")"
  kill_term "$term"; sleep 0.4
  local delta_cs=$((after - before))
  [ "$delta_cs" -lt 0 ] && delta_cs=0
  echo "$((delta_cs * 10))"
}

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

# Record how each terminal's config was isolated to fresh-install defaults
# (H3 fix, 2026-07-16 — see METHODOLOGY.md "Config isolation").
for t in $SELECTED; do
  case "$t" in
    noa)     emit noa meta - - isolation "XDG_CONFIG_HOME=iso-dir (fresh defaults; iso config carries only window-save-state=never to protect the user's session.json)" note ;;
    ghostty) emit ghostty meta - - isolation "--config-default-files=false (no config files read) + --window-save-state=never (phantom saved-state window suppressed)" note ;;
    termy)   emit termy meta - - isolation "XDG_CONFIG_HOME=iso-dir (fresh defaults; verified: writes its default config.txt into the iso dir)" note ;;
    kitty)   emit kitty meta - - isolation "--config NONE (built-in defaults) + -o confirm_os_window_close=0 (harness kill-safety, disclosed)" note ;;
  esac
done

# Record noa build provenance: the version string alone cannot distinguish two
# builds of the same version (observed: two binaries both "Noa 0.1.4" idling
# 205 vs 88 MiB), which makes cross-run memory comparisons unattributable.
case " $SELECTED " in *" noa "*)
  emit noa meta - - noa_bin "$NOA_BIN" path
  emit noa meta - - noa_bin_sha256 "$(shasum -a 256 "$NOA_BIN" | awk '{print $1}')" hash
  emit noa meta - - noa_bin_mtime "$(stat -f%Sm -t '%Y-%m-%dT%H:%M:%S' "$NOA_BIN")" time
  ;;
esac

# ── THROUGHPUT ─────────────────────────────────────────────────────
if axis_selected throughput; then
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
fi  # axis: throughput

# ── FRAME / SCROLL ─────────────────────────────────────────────────
if axis_selected scroll; then
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
fi  # axis: scroll

# Latency + startup are condition-independent; skip them under --equalize
# (the equalized re-run targets the render-sensitive throughput+scroll axes).
if [ "$EQUALIZE" != 1 ]; then
# ── INPUT LATENCY (DSR round-trip proxy) ───────────────────────────
# Statistical budget (H2 fix, 2026-07-16): LAT_RUNS independent process
# launches x LAT_ITERS kept iterations each (LAT_WARMUP discarded per
# launch). Each launch's raw samples are pooled and the reported
# median/p95/p99/max are computed over the POOLED distribution — a p99 from
# 200 iterations rests on ~2 tail samples and is statistically meaningless;
# the pooled full-run budget (>=10 launches x 1000 = >=10000 kept samples)
# gives the p99 ~100 tail samples across independent launches, so
# launch-phase-locked timer collisions can't dominate it.
if axis_selected latency; then
export NOA_PROBE_ITERS="$LAT_ITERS" NOA_PROBE_WARMUP="$LAT_WARMUP"
for term in $SELECTED; do
  echo "[latency] $term ($LAT_RUNS launches x $LAT_ITERS iters, $LAT_WARMUP warmup discarded/launch)"
  pooled_f="$RUNTMP/${term}.lat_pooled"; : > "$pooled_f"
  got=0
  for r in $(seq 1 $LAT_RUNS); do
    export NOA_SAMPLES="$RUNTMP/${term}.lat_samples.$r"
    rm -f "$NOA_SAMPLES"
    out="$(run_once "$term" latency "$LAT_TIMEOUT")" || continue
    set -- $out
    med_ns="${1:-0}"; p95_ns="${2:-0}"; p99_ns="${3:-0}"; max_ns="${4:-0}"; min_ns="${5:-0}"; cnt="${6:-0}"
    if [ "$cnt" -eq 0 ]; then continue; fi
    got=$((got + 1))
    emit "$term" latency - "$r" median_ns "$med_ns" ns
    emit "$term" latency - "$r" p95_ns "$p95_ns" ns
    emit "$term" latency - "$r" p99_ns "$p99_ns" ns
    emit "$term" latency - "$r" max_ns "$max_ns" ns
    emit "$term" latency - "$r" min_ns "$min_ns" ns
    emit "$term" latency - "$r" kept_iterations "$cnt" count
    cat "$NOA_SAMPLES" >> "$pooled_f" 2>/dev/null
  done
  unset NOA_SAMPLES
  if [ "$got" -ge 1 ]; then
    set -- $(pooled_stats "$pooled_f")
    pmed="${1:-0}"; pp95="${2:-0}"; pp99="${3:-0}"; pmax="${4:-0}"; pcnt="${5:-0}"
    emit "$term" latency - pooled pooled_median_ns "$pmed" ns
    emit "$term" latency - pooled pooled_p95_ns "$pp95" ns
    emit "$term" latency - pooled pooled_p99_ns "$pp99" ns
    emit "$term" latency - pooled pooled_max_ns "$pmax" ns
    emit "$term" latency - pooled pooled_count "$pcnt" count
    emit "$term" latency - pooled pooled_launches "$got" count
    # legacy metric names stay populated (from the pooled distribution) so
    # older consumers of results.json/raw.tsv keep working
    emit "$term" latency - median median_ns "$pmed" ns
    emit "$term" latency - median p99_ns "$pp99" ns
    mus=$(awk -v ns="$pmed" 'BEGIN{printf "%.1f", ns/1000}')
    p99us=$(awk -v ns="$pp99" 'BEGIN{printf "%.1f", ns/1000}')
    echo "    pooled ($pcnt samples / $got launches): median ${mus} us, p99 ${p99us} us"
  else
    emit "$term" latency - - status UNMEASURED "no-dsr-reply-within-timeout-render-thread-gated"
    echo "    UNMEASURED (no DSR reply within timeout — reply appears render-thread/focus-gated)"
  fi
done
fi  # axis: latency

# ── WARM STARTUP ───────────────────────────────────────────────────
if axis_selected startup; then
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
fi  # axis: startup

# ── MEMORY (idle / scrollback / multitab / longevity footprint) ────
# Every scenario is dual-reported: active (t=${MEM_ACTIVE_AT_S}s, in-use
# footprint) + settled (median of the last 3 samples of the trajectory to
# t=${MEM_SETTLED_UNTIL_S}s, long-lived idle footprint past GPU-pool reclaim).
# table.md ranks on SETTLED; the full trajectory lands in raw.tsv.
if axis_selected memory; then
for term in $SELECTED; do
  echo "[memory/idle] $term (active @${MEM_ACTIVE_AT_S}s, settled @${MEM_SETTLED_UNTIL_S}s)"
  out="$(run_mem_idle "$term")"
  set -- $out
  a="${1:-}"; s="${2:-}"
  if [ -n "$a" ] && [ "$a" != 0 ]; then
    emit "$term" memory idle - active_bytes "$a" bytes
    emit "$term" memory idle - settled_bytes "$s" bytes
    echo "    active $((a / 1048576)) MiB / settled $((s / 1048576)) MiB"
  else
    emit "$term" memory idle - status UNMEASURED "no-pids-or-timeout"
    echo "    UNMEASURED"
  fi

  echo "[memory/scrollback] $term (active @${MEM_ACTIVE_AT_S}s, settled @${MEM_SETTLED_UNTIL_S}s)"
  out="$(run_mem_scrollback "$term")"
  set -- $out
  a="${1:-}"; s="${2:-}"
  if [ -n "$a" ] && [ "$a" != 0 ]; then
    emit "$term" memory scrollback - active_bytes "$a" bytes
    emit "$term" memory scrollback - settled_bytes "$s" bytes
    echo "    active $((a / 1048576)) MiB / settled $((s / 1048576)) MiB"
  else
    emit "$term" memory scrollback - status UNMEASURED "no-pids-or-timeout"
    echo "    UNMEASURED"
  fi

  echo "[memory/multitab] $term ($MEM_MULTITAB_N windows requested; windows stay open through the settle)"
  out="$(run_mem_multitab "$term")"
  set -- $out
  a="${1:-}"; s="${2:-}"; nproc="${3:-0}"; nwin="${4:-0}"; breakdown="${5:-unknown}"
  if [ -n "$a" ] && [ "$a" != 0 ]; then
    emit "$term" memory multitab - active_bytes "$a" bytes
    emit "$term" memory multitab - settled_bytes "$s" bytes
    emit "$term" memory multitab - windows_requested "$MEM_MULTITAB_N" count
    emit "$term" memory multitab - windows_observed "$nwin" count
    emit "$term" memory multitab - processes_observed "$nproc" count
    emit "$term" memory multitab - proc_breakdown "$breakdown" note
    echo "    active $((a / 1048576)) MiB / settled $((s / 1048576)) MiB, $nwin window(s) observed / $MEM_MULTITAB_N requested, $nproc process(es): $breakdown"
  else
    emit "$term" memory multitab - status UNMEASURED "no-pids-found"
    echo "    UNMEASURED"
  fi

  echo "[memory/longevity] $term ($MEM_LONGEVITY_CYCLES cycles, then final settle @${MEM_SETTLED_UNTIL_S}s)"
  out="$(run_mem_longevity "$term")"
  samples="$(printf '%s\n' "$out" | sed -n 1p)"
  finals="$(printf '%s\n' "$out" | sed -n 's/^final //p')"
  n=0; first=""; last=""
  for b in $samples; do
    n=$((n + 1))
    emit "$term" memory longevity "cycle$n" rss_bytes "$b" bytes
    [ -z "$first" ] && first="$b"
    last="$b"
  done
  if [ "$n" -ge 2 ]; then
    growth="$(awk -v f="$first" -v l="$last" -v n="$n" 'BEGIN{printf "%.0f", (l - f) / (n - 1)}')"
    # Three longevity readings, all first-class (see METHODOLOGY.md): growth
    # rate answers "does it leak?", final-active answers "where does it sit
    # under churn?" (last cycle sample, short idle only), final-settled
    # answers "where does it sit once churn stops?" (>= the full settle of
    # quiescence after the last cycle, past GPU-pool reclaim).
    emit "$term" memory longevity - growth_per_cycle_bytes "$growth" bytes
    emit "$term" memory longevity - final_active_bytes "$last" bytes
    fs=""
    if [ -n "$finals" ]; then
      set -- $finals
      fs="${2:-}"
      [ -n "$fs" ] && [ "$fs" != 0 ] && emit "$term" memory longevity - final_settled_bytes "$fs" bytes
    fi
    if [ -n "$fs" ] && [ "$fs" != 0 ]; then fsdisp="$((fs / 1048576)) MiB"; else fsdisp="UNMEASURED"; fi
    echo "    $n cycles, $((first / 1048576))->$((last / 1048576)) MiB (~$((growth / 1024)) KiB/cycle), final settled $fsdisp"
  else
    emit "$term" memory longevity - status UNMEASURED "fewer-than-2-cycles-completed"
    echo "    UNMEASURED"
  fi
done
fi  # axis: memory

# ── LOAD (idle CPU% + active CPU-time-per-workload) ─────────────────
if axis_selected load; then
for term in $SELECTED; do
  echo "[load/idle] $term (settle ${LOAD_IDLE_SETTLE_S}s discarded, then ${LOAD_IDLE_S}s @ ~1Hz)"
  out="$(run_load_idle_sample "$term")"
  if [ -n "$out" ]; then
    set -- $out
    mean="${1:-0}"; max="${2:-0}"; csw="${3:-0}"
    emit "$term" load idle - cpu_pct_mean "$mean" pct
    emit "$term" load idle - cpu_pct_max "$max" pct
    emit "$term" load idle - settle_discarded_s "$LOAD_IDLE_SETTLE_S" s
    emit "$term" load idle - csw_per_s "$csw" per_s
    emit "$term" load idle - wakeups N/A "not-exposed-by-top--stats-on-this-macos-build;csw_per_s-is-the-proxy"
    emit "$term" load idle - power N/A "powermetrics-needs-sudo,no-passwordless-sudo-available"
    echo "    mean ${mean}% / max ${max}% CPU (settled), ${csw} csw/s (wakeups proxy)"
  else
    emit "$term" load idle - status UNMEASURED "no-pids-or-timeout"
    echo "    UNMEASURED"
  fi

  echo "[load/active] $term"
  abytes=$(stat -f%z "$ASCII")
  out="$(run_load_active "$term" throughput "cat '$ASCII'" "$TP_TIMEOUT")"
  if [ -n "$out" ]; then
    emit "$term" load throughput - cpu_ms "$out" ms
    permib="$(awk -v ms="$out" -v b="$abytes" 'BEGIN{ mib=b/1048576; if(mib>0) printf "%.3f", ms/mib; else print 0 }')"
    emit "$term" load throughput - cpu_ms_per_mib "$permib" ms_mib
    echo "    throughput workload: ${out} ms cpu (${permib} ms/MiB)"
  else
    emit "$term" load throughput - status UNMEASURED "timeout"
    echo "    UNMEASURED"
  fi

  sbytes=$(stat -f%z "$SCROLLF")
  out="$(run_load_active "$term" scroll "cat '$SCROLLF'" "$SCROLL_TIMEOUT")"
  if [ -n "$out" ]; then
    emit "$term" load scroll - cpu_ms "$out" ms
    permib="$(awk -v ms="$out" -v b="$sbytes" 'BEGIN{ mib=b/1048576; if(mib>0) printf "%.3f", ms/mib; else print 0 }')"
    emit "$term" load scroll - cpu_ms_per_mib "$permib" ms_mib
    echo "    scroll workload: ${out} ms cpu (${permib} ms/MiB)"
  else
    emit "$term" load scroll - status UNMEASURED "timeout"
    echo "    UNMEASURED"
  fi
done
fi  # axis: load

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

# ── contention bookends (end of run) ───────────────────────────────
emit harness meta - - loadavg_end "$(sysctl -n vm.loadavg 2>/dev/null)" note
emit harness meta - - uptime_end "$(uptime)" note
BUILDERS_END="$(builders_alive)"
if [ -n "$BUILDERS_END" ]; then
  emit harness meta - - builders_at_end "ALIVE: $BUILDERS_END" note
  echo "WARNING: builders were alive at run END: $BUILDERS_END (results may be contaminated)"
else
  emit harness meta - - builders_at_end "none" note
fi

# ── aggregate → json + markdown ────────────────────────────────────
python3 "$BENCH_DIR/aggregate.py" "$RAW" "$OUT_DIR" "$TS"
cp "$BENCH_DIR/METHODOLOGY.md" "$OUT_DIR/METHODOLOGY.md" 2>/dev/null || true
echo
echo "==================================================================="
cat "$OUT_DIR/table.md"
echo "==================================================================="
echo "JSON:  $OUT_DIR/results.json"
echo "Table: $OUT_DIR/table.md"
