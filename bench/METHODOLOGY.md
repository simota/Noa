# Benchmark Methodology

A reproducible 7-axis comparison of terminal emulators on macOS/Apple Silicon.
Everything is driven by `bench/run_all.sh`; a third party can re-run the whole
suite with one command and get a machine-readable `results.json` plus a
human-readable `table.md` under `bench/results/<timestamp>/`.

## Terminals under test

| Terminal | How it is launched | Exec mechanism |
|---|---|---|
| **noa** (this repo) | `XDG_CONFIG_HOME=<iso> noa --cols 120 --rows 40 -e <wrapper>` | native `-e` (2026-07; older builds fall back to `$SHELL` = wrapper, auto-detected) |
| Ghostty | `ghostty --config-default-files=false --window-save-state=never -e <wrapper>` | native `-e` |
| Termy | `XDG_CONFIG_HOME=<iso> termy` with `$SHELL` = wrapper | `$SHELL` (no `-e` flag exists) |
| kitty | `kitty --config NONE -o confirm_os_window_close=0 <wrapper>` | trailing command arg |
| Alacritty | `XDG_CONFIG_HOME=<iso> alacritty -e <wrapper>` | native `-e` (entry activates when installed) |
| iTerm2 | own instance launched directly, then AppleScript `create window with default profile command <env-wrapper>` | AppleScript (no `-e`; `$SHELL` verified ignored) — see caveats below |
| Warp | `SHELL=<wrapper> Warp` | `$SHELL` — **UNVERIFIED**; **opt-in only** (excluded from the default set; include explicitly via `--only ...,warp`). UNMEASURED axes mean Warp ignored `$SHELL` |
| Terminal.app | own second instance launched directly with `<wrapper>` as document argument | document-open (verified: real tty, env inherited via `login -p`) |
| Rio | `RIO_CONFIG_HOME=<iso> rio -e <wrapper>` | native `-e` (Rio ignores `XDG_CONFIG_HOME`; `RIO_CONFIG_HOME` verified via `--write-config`; iso config pins `confirm-before-quit=false`, kitty-precedent kill-safety) |

The `<iso>` dir / `--config`-suppression flags are the **config isolation**
mechanism (fresh-install defaults for every terminal) and
`--window-save-state=never` is the **Ghostty phantom-window fix** — both
added 2026-07-16 and documented in their own sections below.

The noa binary defaults to `<repo>/target/release/noa` and can be overridden
with the `NOA_BIN` env var (for measuring a candidate build). `--axes
throughput,scroll,latency,startup,memory,load,fire` (any subset; this is also
the default set) restricts a run to those axes.

Versions and machine details are captured automatically into `results.json`
(`terminal_versions`, `machine`) at run time — read them there for the exact
build numbers of the run you are looking at. At authoring time: Ghostty 1.3.1,
Termy 0.2.21, kitty 0.47.4, noa release build of the current branch.

### The 2026-07-17 additions (Alacritty / iTerm2 / Warp / Terminal.app)

Four more terminals have launch entries; each activates only when installed
(`skip (not installed)` otherwise — never faked). What was **verified on this
machine** vs assumed:

- **Alacritty** (not installed at authoring): standard `-e` launch; isolation
  via `XDG_CONFIG_HOME` → iso dir containing an **empty `alacritty.toml`**,
  so the first config candidate wins and a user's `~/.alacritty.toml`
  fallback is never consulted.
- **iTerm2** (verified 2026-07-17): `$SHELL` is **ignored** (the default
  profile execs the passwd login shell), so the pty child is created via
  AppleScript `create window with default profile command` pointing at a
  generated **env-wrapper** (AppleScript-launched commands get launchd's
  env, not ours — verified; the wrapper re-exports `NOA_*` and execs
  `wrapper.sh`; command runs on a real tty — verified). Three caveats, all
  disclosed per run: (1) the AppleScript targets the app **by name**, so the
  harness **skips iTerm2 entirely if a user instance is already running**
  (collateral-safety invariant); (2) prefs are macOS defaults
  (com.googlecode.iterm2) and are **not isolable** without mutating user
  defaults — iTerm2 runs with the user's prefs, recorded in
  `results.json.isolation`; (3) our instance may open restored/startup
  windows running login shells alongside the command window (verified: 3
  windows on this machine) — a Ghostty-phantom-class contamination of the
  memory/load axes that is visible in `multitab_procs`/`wincount` rather
  than silently included; read iTerm2's memory/load numbers with that in
  mind. Its startup number reflects the AppleScript window path, not a
  plain process launch.
- **Warp**: launch entry uses `SHELL=<wrapper>`, **unverified** — if Warp
  resolves the shell from user records instead, every axis reports
  UNMEASURED (honest failure). Prefs and account state are not isolable;
  disclosed. **Opt-in only**: excluded from the default terminal set —
  include it explicitly with `--only ...,warp`.
- **Terminal.app** (verified 2026-07-17): launched as **our own second
  instance** by exec'ing the app binary directly with the wrapper path as
  document argument — verified to coexist with the user's running Terminal
  (distinct pid, untouched), give the wrapper a real tty, and pass our
  environment through its `login -p` wrapper. Never launched via
  `open`/AppleScript (those route to the user's instance). Prefs
  (com.apple.Terminal) are not isolable; disclosed.

For iTerm2/Warp/Terminal.app the equalized re-run records
`NATIVE-DEFAULT(prefs-based)` — they expose no CLI size/font control.

## The uniform-workload design

The single biggest fairness lever is that **every terminal runs the exact same
pty child** — `bench/wrapper.sh` — regardless of how it is told to do so. noa,
Ghostty, and kitty accept the wrapper as an explicit command (`-e` / trailing
arg); Termy has no `-e`/command flag, so the wrapper is installed as `$SHELL`
(Termy spawns `$SHELL` as the pty child). The workload logic, timestamps, and
sentinels are then identical across all four. The only thing that differs is
the OS-level launch verb, which is unavoidable and disclosed above.

Because a custom-named shell is used, none of the terminals inject their
shell-integration (OSC 133/7) hooks, so that variable is held constant too.

Each measured run: kill any previous *harness-launched* instance of that
terminal → export the workload env → record `t0` (monotonic ns) → launch a
**fresh** process → poll for the sentinel file → record `t1` → kill it.
Fresh-process-per-run avoids single-instance handoff hiding the work in an
already-warm process.

## Zero-collateral process lifecycle (PID-scoped)

The harness **never kills, samples, or activates a process by name or path**
(grep-verifiable: no `pkill`/`killall` anywhere under `bench/`). Every pid it
spawns is recorded in a per-terminal registry; lifecycle and measurement
operate only on registered pids and their descendant trees:

- **Kill**: `kill_term` resolves the registered pids that are still alive
  *and still run the binary the harness launched* (a `ps -o comm=` identity
  check guards against pid reuse), expands them to their full descendant tree
  (children are enumerated *before* the root dies, so the wrapper's
  `sleep 86400` hold-child can't escape as a reparented orphan), sends
  SIGTERM, waits up to 2s for the tree to exit, then SIGKILLs survivors. The
  escalation matters: a terminal whose pty child is still alive may sit on a
  confirm-close dialog instead of exiting on SIGTERM, and a half-dead stale
  instance both pollutes the next sample and can block the next launch.
- **Measure**: `app_pids` (memory/load axes) is the same registry-rooted tree
  — a name-based `pgrep` match would silently sum a user's own running
  instance of the same terminal into the RSS/CPU numbers.
- **Activate** (latency axis focus, needed by render-gated DSR responders):
  targets the tracked pid via System Events `unix id`; the `open -a` fallback
  (focus-only, can never kill) fires only while our instance is alive.
- An EXIT trap kills all still-registered pids, so an aborted run leaves
  nothing behind.
- Window counting (`tools/wincount`) matches windows by **owner pid**, not
  owner name, for the same reason.

Consequence: a bench run is safe to execute while the user's own Ghostty /
kitty / Termy / noa sessions are open — they are invisible to the harness.
(Do still run decisive measurements on an otherwise quiet machine; that is a
noise concern, not a safety one — and since 2026-07-16 the harness enforces
it, see "Builder-quiescence gate" below.)

## Config isolation — fresh-install defaults for everyone (2026-07-16)

The scored comparison is **each terminal at its fresh-install defaults**.
Before this fix, every terminal launched with the *user's* real config —
which made the comparison depend on whatever this machine's dotfiles happen
to contain. Concretely: **noa's previous numbers carried
`server-enable = true` from the user's real `~/.config/noa/config`** (a
JSON-RPC/WS server thread + listener that no competitor was running — a
memory/load handicap unique to noa), and Ghostty/Termy carried the user's
theme/font overrides. None of that belongs in a cross-terminal comparison.

Mechanism per terminal (each **verified**, not assumed):

| Terminal | Isolation | Verified how |
|---|---|---|
| noa | `XDG_CONFIG_HOME` → per-run empty temp dir | `noa-config` resolves config from `$XDG_CONFIG_HOME` (`crates/noa-config/src/lib.rs: xdg_config_dir`); with the iso dir it finds no user config and applies built-in defaults |
| Ghostty | `--config-default-files=false` | Ghostty's documented flag: reads **no** config files at all. XDG alone would NOT isolate it — Ghostty also reads `~/Library/Application Support/com.mitchellh.ghostty/config` (where this machine's real config lives), which an XDG redirect does not cover |
| Termy | `XDG_CONFIG_HOME` → the same iso dir | verified empirically 2026-07-16: launched with an isolated `XDG_CONFIG_HOME`, Termy **creates its fresh default `termy/config.txt` inside the iso dir** and never opens the user's `~/.config/termy/config.txt` |
| kitty | `--config NONE` | kitty's documented "use built-in defaults" mode; `-o confirm_os_window_close=0` stays as the one deliberate non-default (a kill must terminate the window, not hang on a confirm dialog — pure harness mechanics, identical effect for the axes measured) |

Two disclosed nuances:

- **noa's iso config is not empty — it contains exactly
  `window-save-state = never`.** Reason: noa persists its session topology to
  `~/Library/Application Support/noa/session.json` (macOS `data_dir`, which no
  environment variable redirects), so a default-config harness instance would
  **overwrite the user's real session file on every exit**. The key is
  measurement-neutral for this suite: session *restore* was already skipped in
  every harness run (noa skips restore when `--cols/--rows` are given —
  `cli_grid_override` in `app/session_restore.rs`), so the only behavior
  change is suppressing a ~200-byte JSON write at exit. Ghostty gets the
  functionally identical `--window-save-state=never` flag (see the phantom
  section below), so the two Ghostty-config-compatible terminals run with the
  same setting. Verified empirically 2026-07-16: `noa +show-config` under the
  iso `XDG_CONFIG_HOME` reports the iso values (`window-save-state = never`
  plus a test key), and a controlled GUI run with the iso config left the
  user's `session.json` mtime untouched.
- The user's real config files are **never read, written, moved, or
  restored-from-backup** by the harness (the pre-2026-07-16 `--equalize` mode
  used to swap the real files with backup/restore; equalized settings now go
  into the iso dir too).

Each terminal's isolation mechanism is recorded per run in
`results.json.isolation` and echoed in `table.md`.

## Ghostty phantom saved-state window — FATAL measurement bug, fixed (2026-07-16)

**Symptom:** on this machine, `ghostty -e <cmd>` opened, *per window*, BOTH
the `-e` command subtree AND an independent interactive shell subtree:

```
ghostty -e sleep 300
├─ login -flp simota sleep 300                                  <- the -e command
└─ login -flp simota /bin/bash --noprofile --norc -c exec -l /bin/zsh
   └─ -/bin/zsh                                                 <- PHANTOM
```

Reproduced on a single window (2026-07-16) and visible in every archived
`multitab_procs.ghostty.txt` (10 windows = 10 wrapper subtrees + **10
`login → -zsh` subtrees**). The phantom is a **macOS window-restoration
surface**: Ghostty's `window-save-state` defaults to `default`, which on
macOS defers to AppKit state restoration — and that restored a
previous-session window (running the default shell = the user's login zsh,
`.zshrc` and all) alongside every `-e` window the harness opened.

**Impact:** every Ghostty memory and load number before 2026-07-16 included
one extra `login` + interactive `zsh` per window (double window count in
multitab), plus that zsh's rc-sourcing CPU in load-active. No other terminal
under test had any analogous stray (noa/Termy: exactly `app + sh + sleep`
per window; kitty: those plus its per-window `kitten` helper, which is real
kitty architecture, correctly charged — verified in the archived
`multitab_procs.<term>.txt` of runs 20260716-114608/121228/131452).

**Fix (option a — suppress at the source):** launch Ghostty with
`--window-save-state=never`. Verified 2026-07-16: with the flag, a single
`ghostty -e sleep 300` spawns exactly one `login → sleep` subtree and no
zsh; without it, the phantom reappears deterministically. No PID-exclusion
special-casing is needed — the measured tree is now symmetric with the other
terminals. (The rejected alternatives: (b) structurally excluding the
`login→zsh` subtree from `app_pids` would have made Ghostty's measurement
rules asymmetric; (c) dual gross/corrected reporting would have kept the
noise in the headline numbers.)

Note the fairness reasoning: the phantom was *not* "Ghostty's own cost" the
way kitty's `kitten` is — it existed only because the bench machine had a
Ghostty saved-state from prior sessions, i.e. it was environment
contamination, not architecture. A fresh-install Ghostty answering
`ghostty -e cmd` does not open it.

The contamination was also **self-reinforcing**: every pre-fix harness
window was torn down by signal (`kill_term`), which macOS records as
restorable state — so bench runs themselves kept re-seeding the phantom for
the next launch. With `--window-save-state=never` Ghostty neither restores
nor records that state; after the first post-fix run, even a flag-less
`ghostty -e` on this machine stopped reproducing the phantom (verified
2026-07-16) because the stale state had been cleared. The flag stays in the
launch line permanently so no future environment state can re-introduce it.

## Fullscreen measurement (2026-07-17)

The render-path axes — **throughput, scroll, fire, latency, and
load-active** — measure every terminal in **native macOS fullscreen** by
default. Rationale: it gives every terminal the exact same physical geometry
without per-terminal grid flags, which also neutralizes the previously
disclosed equalization gaps (Termy/iTerm2/Warp/Terminal.app expose no size
control; noa was pinned to 120×40 while others ran native defaults).

Mechanism, per measured launch:

1. Launch as usual; poll pid-scoped `wincount` until a window exists.
2. **Native-flag terminals** (kitty `--start-as=fullscreen`, Alacritty
   `-o window.startup_mode="Fullscreen"`) are launched fullscreen directly
   — reliable, needs no Accessibility; the harness only waits out the Space
   animation. (An AX verify on them can false-negative: kitty was observed
   at a clearly-fullscreen 244×85 region while the AX read reported
   failure.) **Ghostty is deliberately NOT native-flagged**: with
   `--fullscreen`, ghostty 1.3.1 never executes its `-e` command (verified
   2026-07-17 — the pty child never starts), so it takes the AX path; its
   `--window-position-x/y` flags are unaffected and still pin the display.
   **The rest** (noa, ghostty, Termy, iTerm2, Warp, Terminal.app) get
   `AXFullScreen` set on window 1 of OUR tracked process via System Events
   `unix id` (never by app name), polled back until `true`, then settled
   0.8 s.
3. **Display pinning:** the target display — the one under the mouse cursor
   at run start (`bench/tools/dispinfo`, recorded as `target_display`) — is
   where every measured window is placed before fullscreening, since macOS
   otherwise places a window (and thus its fullscreen Space) on whatever
   display that app last used. Ghostty gets `--window-position-x/y` flags,
   Alacritty `window.position` via `-o`; the AX-path terminals get an
   AXPosition set before AXFullScreen. kitty exposes no position control —
   it fullscreens on the display where its window opens (normally the
   active one). An AX read that never confirms is no longer treated as
   failure (the set can succeed while the read is denied — observed); the
   per-rep fire `region` remains the ground truth.
4. Only then create the **gate file** (`NOA_GO`): the wrapper holds the
   workload until the gate appears, so no bytes are consumed at pre-
   fullscreen geometry. `load-active` additionally takes its "before" CPU
   snapshot after the fullscreen settle and releases the gate afterwards,
   keeping the transition's own CPU out of the "same work, less CPU" delta.

Not fullscreen: **startup** (measures the plain launch itself; its sentinel
predates any window manipulation), **memory** and **load-idle** (multitab's
`wincount` fairness check counts on-screen layer-0 windows, which fullscreen
Spaces would hide; keeping the whole memory axis windowed keeps its scenarios
mutually consistent), and **--equalize** runs (pinned-grid geometry is the
point there).

Setting `AXFullScreen` requires Accessibility permission for `osascript`.
The harness probes this **once, up front, all-or-nothing**: if denied, it
prints a warning and measures EVERY terminal windowed — a run never mixes
fullscreen and windowed terminals — and records the outcome in
`results.json.contention`-adjacent meta (`fullscreen` key). Per-window
failures (an app without the attribute) fall back to windowed for that rep
and are emitted as `fullscreen_window FAILED` meta rows rather than silently
passing.

Numbers from fullscreen runs are not comparable to pre-2026-07-17 windowed
runs (visible grid area differs); as always, compare within one results
directory only.

## The four axes

### 1. Throughput
Time for the terminal to consume/display `150MB_ascii.txt` and
`150MB_unicode.txt` (mixed CJK/emoji/CSI). The wrapper brackets `cat <file>`
with monotonic timestamps taken *inside* the pty child, so the reported number
excludes process-spawn and window-creation overhead.

Completion is defined by `cat` returning. This is a sound proxy because the pty
has a small (~16 KiB) kernel buffer with flow control: `cat` blocks on `write`
until the terminal drains the buffer, so `cat` cannot finish until the terminal
has read essentially all 150 MB. The last buffer-full may still be mid-parse
when the timer stops — negligible against 150 MB. Reported as median MiB/s over
repetitions (`--quick` = 1 rep, full = 3).

**Caveat:** this measures the read+parse (and enqueue-to-GPU) path under flow
control, not necessarily every glyph reaching the screen. It is the same
"consume the pipe" metric terminal-throughput shootouts conventionally use, and
it is identical across all terminals here.

### 2. Input latency (DSR round-trip proxy)
`bench/tools/dsr_probe.c` runs as the pty child: it puts the tty in raw mode
and performs {write `ESC[6n`, read the `ESC[…R` cursor-position reply}
iterations, timing each with `CLOCK_MONOTONIC`.

**Statistical budget (upgraded 2026-07-16):** a full run is **10 independent
process launches × 1000 kept iterations each** (100 warm-up iterations
discarded per launch — proportional to the old 200/20), and every kept raw
sample is written out and **pooled across launches**; the reported
**median / p95 / p99 / max** are computed over the pooled ≥10000-sample
distribution (nearest-rank percentiles, same convention as the per-run
numbers, which stay in `raw.tsv` for auditing). The old budget — 2 launches
× 200 iterations, per-run p99s medianed — rested its p99 on ~2 tail samples
per launch, which is noise, and could be dominated by a single unlucky
timer collision in one launch. With the pooled budget the p99 rests on
~100 tail samples spread over 10 independent process lifetimes.
`--quick` uses 2 launches × 200 iterations (smoke only; its tail stats are
indicative, not decisive).

**Desynced focus schedule:** the latency axis focuses the window shortly
after launch (see below). The two pre-activation delays are randomized
per launch (±0.7 s around the old fixed 1.0 s/1.5 s points, via `$RANDOM`)
— a deterministic schedule phase-locks the activation-triggered redraw to
the same probe iterations in every run, which can systematically create
(or hide) tail collisions with the app's periodic timers.

**What this is and isn't:** it measures the **pty → VT parser → responder → pty**
loop — the same parse-and-respond path a keystroke echo travels — fully
automatable and reproducible. It is **not** photon-to-glass or keyboard-to-glass
latency; there is no display-refresh or input-hardware component. Treat it as a
parser-responsiveness proxy, not a "feel" latency.

For **noa** specifically, DA/DSR replies are queued in the terminal model and
drained via `Terminal::take_pending_writes()` on the io thread (see
`crates/noa-grid` + `noa-app/src/io_thread.rs`); the proxy exercises exactly
that path. No other terminal here exposes internal latency instrumentation, so
the DSR proxy is the only apples-to-apples number available.

**Focus-gated responders (Termy):** the harness activates each terminal's
window (`open -a` / System Events) shortly after launch during this axis,
because Termy only answers DSR from its render path and only while its window
is focused — unfocused, it never replies and was previously reported
UNMEASURED. With activation Termy produces a real number, but read it with the
architectural difference in mind: an event-driven responder (noa µs-scale,
Ghostty tens of µs) answers as soon as the bytes are parsed, while a
frame-scheduled responder (Termy ~one display refresh, kitty ms-scale) binds
the reply to its render/vsync cadence. The µs-vs-ms gap is an architecture
statement, not a measurement artifact — but it also genuinely bounds how fast
such a terminal can ever turn input into a visible response.

noa also ships an env-gated end-to-end tracer (`NOA_LATENCY_TRACE=1`, stderr:
`key→present` per keystroke from winit key-event receipt to `wgpu` present of
the frame containing the echo). It is noa-internal — usable as a photon-proxy
sanity check for noa itself, not for cross-terminal comparison.

### 3. Frame / scroll
`scroll_stress.txt` (~40 MB, generated by `gen_scroll.py`): every line repaints
with a fresh 256-color SGR fg/bg + attribute transition, and every 500 lines a
`DECSTBM` scrolling region is set and the cursor homed, so the terminal
exercises scroll-region handling rather than a single flat append-scroll. Timed
the same way as throughput (bracketed `cat`). Reported as median wall-time (ms)
and MiB/s — a render-pressure proxy, since SGR churn + scroll-region resets are
render-bound work on top of raw parsing.

### 4. Warm startup
Process launch → pty child begins executing. The wrapper writes the sentinel the
instant it starts, so `t1 − t0` is spawn → (window created + pty ready + child
exec). One unrecorded warm-up run precedes ≥5 recorded reps (median reported).
"Warm" = binaries/dylibs already in the OS page cache from prior runs.

**Caveat:** this captures time-to-pty-ready, which for a GUI terminal closely
tracks time-to-window, but is not a pixel-level "first frame painted" measure.
It does not run an interactive shell rc (the wrapper is the child), so it
excludes user shell-startup cost — deliberately, to isolate the *terminal's*
startup from `.zshrc`.

### 5. Memory — idle / scrollback / multitab / longevity

**Metric: macOS "physical footprint"**, not plain RSS. Read via the system
`footprint` tool (`footprint -j <file> -f bytes <pids...>`, summing the JSON
`processes[].footprint` field). This is the same number Activity Monitor's
"Memory" column shows — dirty + compressed + IOSurface/GPU-owned memory — and
matters here specifically because all four terminals are GPU-accelerated
(Metal/wgpu-backed): plain `ps`-reported RSS undercounts them by 2-3x (e.g. one
spot check: Ghostty RSS 241 MB vs footprint 718 MB for the same process at the
same instant — the gap is IOSurface + IOAccelerator (graphics) categories that
`ps` doesn't see at all).

**"Belongs to the app" = process tree rooted at the pids THIS harness
launched** (the PID registry described above), expanded to the full
descendant tree via repeated `ps -axo pid,ppid` scans (`bench/run_all.sh:
pid_tree_multi`/`app_pids`). This is robust to both multi-process
architectures seen here — helper processes per window/tab (Ghostty, kitty)
and plain one-process-per-window (noa, Termy) — while guaranteeing that a
user's own concurrently-running instance of the same terminal is never summed
into the measurement (the previous name-based `pgrep` matching could do
exactly that). The pty child (`wrapper.sh` and whatever it runs) is included
as a descendant of every terminal, but it is the exact same tiny script for
all four, so it adds an equal, negligible constant everywhere — it does not
bias the comparison.

Four scenarios, one launch mode: the harness adds `NOA_HOLD=1` to the pty
child's env (`wrapper.sh`), which makes it `sleep 86400` after its own work
instead of exiting — otherwise a terminal that closes its window when the pty
child exits would vanish before the harness could sample it.

#### Memory: two numbers per scenario (active / settled)

**Why one sample is a coin flip — GPU-driver-pool reclaim is
nondeterministic.** All four terminals are GPU-accelerated, and on macOS the
Metal/driver-owned memory pools ("Owned physical footprint (unmapped)
(graphics)" in vmmap terms) are **auto-reclaimed by the driver 5–40 s after
the last GPU submit** — the timing is driver-scheduled, varies run to run,
and moves the footprint by **±10–80 MB per process**. This was established
with a dedicated stage-by-stage probe,
`crates/noa-render/examples/mem_probe.rs` (commit `c412852`): device creation
costs ~192 KB and a full pipeline + one offscreen draw ~4.3 MB — the ~95 MB
idle pool in the real app comes from the windowed present path, decays on its
own after ~5–40 s without submits, and is unaffected by `wgpu::Limits`
right-sizing. The consequence for benchmarking: a single blind
`sleep N; sample` lands randomly **before or after** the reclaim. Observed
across otherwise-identical harness runs: the **same noa binary** measured
87.6 MiB idle (run `20260716-084038`) vs 211.8 MiB (run `20260716-114608`),
and Ghostty similarly 170.4 → 253.5 MiB. Neither number is wrong — they are
two different points on the same decay curve.

**The fix: every memory scenario reports two labeled values, uniformly for
all four terminals** (`bench/run_all.sh: mem_dual_sample`):

- **active** — footprint at **t = 15 s** after the scenario's work
  (`MEM_ACTIVE_AT_S`; 3 s in `--quick`). This is the *in-use footprint*: what
  the terminal occupies while it is actually being exercised, GPU pools still
  resident.
- **settled** — the *long-lived idle footprint*: footprint after full
  quiescence, past the reclaim window. Rather than a second blind sleep, the
  harness samples the process tree's footprint **every 5 s from t = 15 s to
  t = 90 s** (`MEM_SAMPLE_EVERY_S` / `MEM_SETTLED_UNTIL_S`; every 5 s to 30 s
  in `--quick`) and reports **settled = median of the last 3 samples**
  (t = 80/85/90 s — ≥ 75 s of quiescence, comfortably past the 5–40 s reclaim
  window; the median absorbs a straggling reclaim step or a background-noise
  blip). The **full trajectory is emitted to `raw.tsv`** (rep column
  `t15s`,`t20s`,…,`t90s`, metric `footprint_bytes`), so both numbers — and
  the reclaim step between them — are auditable after the fact.

**Ranking uses the settled value** (`table.md`, "noa rank per axis"), with
active always shown alongside. Rationale: a terminal's steady state is what
long-lived sessions pay for — terminals are typically open for hours to
weeks, so the footprint that persists after the driver reclaims its transient
pools is the honest "cost of keeping it around"; the active reading remains
first-class data (it is what you pay *during* heavy use) but ranking on it
would rank driver-reclaim timing luck, not the terminals.

- **mem-idle**: launch → dual-sample (active @ 15 s, trajectory to 90 s,
  settled = median of last 3).

  **Exact metric definition (for reproducibility):** the reported numbers are
  the sum of `footprint -f bytes` "phys footprint" over the harness-launched
  process tree — the terminal process(es) *plus* the held pty child
  (`wrapper.sh`'s `sh` + `sleep`, a few hundred KiB, identical for every
  terminal) — for a single window (noa pinned to 120×40 via CLI; others at
  their native default size), with each terminal at fresh-install defaults
  (see "Config isolation"; runs before 2026-07-16 used the user's real
  configs), sampled on the fixed clock above.

  **Why config parity matters (measured):** before isolation, noa was the
  only terminal benched with the developer's personal config — which set a
  rotating `background-image` (`background-image-interval = 60`: a decode +
  2 s GPU crossfade fired at t=60 s *inside the idle sampling window*,
  spiking footprint ~+116 MiB and leaving ~+17 MiB settled),
  `server-enable = true`, and a `client-remote`. noa's isolation is via
  `XDG_CONFIG_HOME` (the iso config carries only the measurement-neutral
  `window-save-state = never` pin); `noa --config-default-files=false`
  exists for manual parity runs. Termy reads its user config either way —
  inspected: a theme plus feature toggles all at/near defaults (tmux
  integration off, debug overlay off, no size/font overrides), nothing that
  schedules work or retains buffers, so it is default-equivalent for these
  axes. It is **not** summed `ps` RSS (that
  undercounts GPU-backed apps 2–3×, see above) and **not** an
  interactive-shell session (no zsh/rc cost is included).

  **Reconciling other measurements of "noa idle memory":** an earlier
  profiling pass reported noa idle at "143 MB" (footprint tool, 8s settle)
  vs this harness's 87.6 MiB baseline. Four factors account for such gaps,
  in order of size: (1) **build identity** — two different noa builds both
  reporting version "Noa 0.1.4" were observed idling at 205 vs 88 MiB, which
  is why the harness now records `NOA_BIN` path + sha256 + mtime into
  `results.json` (`noa_bin`); (2) **GPU-pool reclaim timing** — the same
  binary reads tens of MiB apart depending on where the sample lands on the
  reclaim curve (the whole point of the dual metric above; an early decay
  check that read a flat 205 MiB at 3/8/15/30 s had simply not crossed that
  instance's reclaim point yet — reclaim needs *sustained* no-submit
  quiescence and its onset varies); (3) **what is measured** — an interactive
  session (login zsh + user rc) vs the harness's inert wrapper child, and
  window/config state; (4) **units** — 143 MB = 136.4 MiB.
- **mem-scrollback**: launch, feed `scroll_stress.txt` (the same ~40 MB SGR/
  scroll-region stress file as axis 3) → dual-sample, clock starting when the
  flood ends. The active reading deliberately includes the post-flood GPU/
  allocator transient (font atlas population, allocator overshoot) — that
  *is* the in-use footprint after heavy output — while the settled reading
  shows what the scrollback actually costs to keep.
- **mem-multitab**: opens `MEM_MULTITAB_N` (10 full run / 3 quick) windows by
  relaunching the terminal that many times, staggered 0.3s apart, then
  dual-samples — **all N windows stay open through the entire trajectory**,
  so the settled reading is N live windows after reclaim, not a wind-down.
  **Fairness instrumentation** (added 2026-07-16): the
  harness verifies what actually materialized rather than assuming it —
  - `windows_observed`: on-screen layer-0 windows owned by the harness's own
    pids at sample time (`tools/wincount`, owner-**pid** matched);
  - `processes_observed` + `proc_breakdown` (`comm=count,...`, full pid/ppid
    detail in `multitab_procs.<term>.txt` in the results dir): the process
    composition of the tree. Process counts legitimately differ per
    architecture — every terminal runs exactly N× the identical pty child
    (`wrapper.sh` = one `sh` + one `sleep` per window); anything beyond that
    (e.g. per-window helper/renderer processes) **is part of that terminal's
    own cost** and is correctly charged to it. The apples-to-apples quantity
    is *total footprint for the same N requested-and-observed windows running
    the same child*, not process count.
  If a terminal reuses one process/window across launches (singleton
  behavior) instead of opening N independent ones, that is reported as-is —
  it's a real architectural difference, not a failure, and is never faked.
- **mem-longevity**: ≥5 (5 full run / 2 quick) repeated flood
  (`scroll_stress.txt`) + **short** idle cycles (3 s between cycles; 1 s in
  `--quick`) **inside one long-lived pty child** (a dedicated
  `NOA_MODE=longevity` loop in `wrapper.sh`) — relaunching per cycle would
  measure N cold starts, not longevity growth. The short inter-cycle idle is
  deliberate: the per-cycle trajectory is the **"under churn"** story
  (sustained heavy use with no time for driver reclaim). After the last
  cycle the harness then runs the same dual-sample trajectory (raw.tsv reps
  `final_t15s`…`final_t90s`) to capture where the process lands once churn
  stops.

  **Longevity is three numbers, all reported** (`table.md`):
  - *growth rate per cycle* — `(last − first) / (cycles − 1)` over the
    per-cycle (under-churn) samples; answers "does sustained flood+idle
    leak?" (0 = flat is ideal; small negative values are
    settling/compression artifacts, i.e. also "no leak", not a bonus);
  - *final active footprint (MiB)* — the last cycle's sample, still under
    churn; answers "what does sustained heavy use cost while it happens?";
  - *final settled footprint (MiB)* — after ≥ 75 s of full quiescence
    following the last cycle (past GPU-pool reclaim); answers "what does a
    long-lived session cost once the burst is over?". **This is the ranked
    reading**, consistent with the settled-ranks-everything rule above.
  Ranking by any one alone misleads: a terminal can be flat but heavy, or
  light but growing. The per-cycle trajectory is printed too so readers can
  check the shape (monotone growth vs one-off settling) themselves. For the
  rank list, growth is ranked as a **leak rate with negative values clamped
  to 0**: a shrinking trajectory (memory returned during settling) means "no
  leak", the same as flat — ranking the raw signed value would reward a
  transient shrink over a genuinely flat profile. The raw signed growth stays
  visible in its table row.

  **Known artifact (RCA'd, not a leak):** noa's small positive growth
  (~+112 KiB/cycle post config-parity; +288 before) is macOS 26 xzone
  allocator dirty-page creep, not retained objects. vmmap diffed at cycle 2
  vs cycle 9 of a 9-cycle run: every region byte-identical except
  `MALLOC_SMALL` +0.5 MiB and `MALLOC_SMALL (empty)` +2.4 MiB — *empty*
  malloc regions whose freed pages stay dirty (xzone quarantine places each
  cycle's frees on fresh pages and returns them "on its own schedule";
  `malloc_zone_pressure_relief` is a documented no-op under xzone, see
  `noa-app/src/memory.rs`). The creep is asymptotic (~0.2%/cycle,
  decelerating; a 10-cycle run showed cycle 10 *dropping* 12 MiB when xzone
  self-reclaimed) and does not survive quiescence — the ranked final-settled
  reading is unaffected. Eagerly flushing between cycles was measured
  counterproductive (see `MEMORY_TRIM_QUIESCENCE` docs). The two
  clamped-negative growth values in the v3 run (termy −12.8, kitty −15.0
  MiB/cycle) are the same class of artifact in the other direction: a
  driver-pool reclaim landing before the cycle-5 sample.

### 6. Load — idle CPU% + active CPU-time-per-workload

- **load-idle** (settled window, uniform for all terminals): launch (hold
  mode) → **discard the first 15s entirely** (5s in `--quick`) → sample
  summed process-tree `ps -o pcpu=` (macOS's own decaying CPU% average, the
  same figure Activity Monitor shows) once per second for 60s (10s in
  `--quick`). Reports mean and max %CPU **of the settled window only**. The
  settle matters: the launch transient (dyld + AppKit window materialization
  + first frames, ~200ms of real work) decays through `pcpu`'s exponential
  average for >10s — the earlier 2s skip let it contaminate both mean and
  max (a prior noa "idle max 17%" was RCA'd to exactly this artifact, not to
  idle-loop behavior).
  **Wakeups: reported via a context-switch proxy (`csw_per_s`), not faked.**
  `top -stats` was tried first per the brief; on this macOS build (26.5.1)
  `top`'s own usage text lists the valid `-stats` keys and neither `wakeups`
  nor `power` is among them (confirmed empirically — passing them makes `top`
  print its usage and exit non-zero). What *is* exposed is `csw` — cumulative
  context switches per process. The harness snapshots summed process-tree
  `csw` at the start and end of the settled idle window and reports the delta
  per second as **`csw_per_s`**. Read it as a **wakeups proxy**: an idle,
  mostly-blocked process performs a context switch each time it wakes to do
  work (timer, display link, event), so idle csw/s tracks idle wakeup rate
  closely — but it is not literally the `power`/wakeups counter Activity
  Monitor shows (a busy-spinning thread could switch without sleeping;
  per-wakeup work is not weighted). `wakeups` itself stays `N/A` with the
  reason recorded. **Power: N/A, not faked** — `powermetrics` can report it,
  but requires `sudo`, and this machine has no passwordless sudo configured;
  per the brief, that means "don't attempt it, don't prompt."
- **load-active**: process-tree cumulative CPU time (`ps -o time=`, parsed as
  user+sys centiseconds, summed per-pid so one dead pid can't abort a whole
  batch) sampled immediately before launch-visible and immediately after the
  standard throughput (`150MB_ascii.txt`) and scroll (`scroll_stress.txt`)
  workloads complete — "same work, less CPU". **Deliberately excludes the
  `cat` child's own CPU time**: the "before" snapshot is taken before `cat`
  exists and the "after" snapshot after it has exited (and been reaped), so
  its cost never enters either side. This is intentional, not an oversight —
  `cat` is identical software run identically by all four terminals and isn't
  part of any terminal's own efficiency; only the terminal process's own
  parse/render CPU is being measured. Reported in ms total and ms-per-MiB
  processed (normalized).

### 7. Fire — DOOM-fire IO stress (fps)

`bench/tools/fire.c` runs as the pty child (`NOA_MODE=fire`): it renders the
classic DOOM fire effect (Fabien Sanglard's algorithm, the workload
popularized as a terminal benchmark by
[DOOM-fire-zig](https://github.com/const-void/DOOM-fire-zig)) as truecolor
half-blocks — every frame repaints the whole region with per-cell
`SGR 38;2/48;2` RGB + `U+2584` and absolute cursor positioning. This is the
"animated TUI at max rate" shape none of the other axes exercise:
frame-structured, truecolor-dense, cursor-repositioning flood. After 60
discarded warmup frames (glyph-atlas population, alt-screen entry) it renders
flat-out for 10 s (3 s in `--quick`) and reports fps; the harness runs 3 reps
(1 in `--quick`) and reports the median.

**Render region follows the run's geometry mode** (recorded per run as
`fire_condition`, per rep as `region`):

- **Fullscreen runs (default): full-window** — the upstream DOOM-fire-zig
  official condition. The fullscreen gate guarantees the winsize read
  happens at final geometry, and every terminal fills the same physical
  screen; the resulting **cell count follows each terminal's own font
  defaults** (exactly like upstream comparisons), so the per-terminal region
  is printed next to every fps number rather than hidden.
- **Windowed fallback runs: fixed 80×24 region** — fps is inversely
  proportional to cell count, so full-window fps on unequal window geometry
  would measure the geometry lottery, not the terminal. The fixed region
  fits every default grid (nothing clips) and gives **every terminal a
  byte-identical stream** (fixed PRNG seed → deterministic frame sequence)
  with constant frame size, so fps maps linearly to drain MiB/s.

The two conditions are not comparable to each other; `table.md` states which
one produced its numbers.

**What fps means here:** producer-side frames/second under pty flow control —
the same "consume the pipe" proxy as axis 1 (the pty's small kernel buffer
blocks the producer's `write` until the terminal drains). It is drain rate,
not photon rate: a display-paced consumer shows up as ~refresh-rate fps (the
signature this axis exists to detect), while an event-driven consumer reports
hundreds or more. The window is focused during the run (same PID-scoped
activation as the latency axis, applied uniformly) so no terminal is measured
under macOS occluded/unfocused throttling.

**Anchor caveat:** published DOOM-fire-zig figures come from other machines
and full-window regions and are **not comparable** to this axis's numbers —
they motivated the axis, nothing more. This implementation reproduces the workload *shape*,
not upstream's exact bytes. As a CPU-bound axis it is covered by the
builder-quiescence gate; it is skipped under `--equalize` (the fixed region
makes it grid/font-independent by construction). Full design rationale:
`docs/specs/bench-doom-fire.md`.

### Ghostty load-active timeout (baseline 2026-07-16) — root-caused & fixed

The 20260716-084038 baseline reported Ghostty's two load-active rows as
`UNMEASURED timeout` (both its `cat` workloads failed to complete within
generous timeouts), uniquely among the four terminals. Investigation findings:

- The pre-fix `kill_term` was `pkill -x ghostty` — it killed only processes
  *named* ghostty, orphaning the `sh` + `sleep 86400` HOLD pty children of
  every memory/load scenario (≈14 orphan pairs accumulated across the
  memory axis by the time load-active ran), and had no SIGKILL escalation
  for an instance that terminates slowly or not at all.
- An isolated SIGTERM to a fresh Ghostty with a live pty child *does*
  terminate it cleanly (tested directly), so the failure required the
  accumulated state of a full sequence, not a single kill.

After the PID-scoped rework (tree enumerated before the kill, TERM → 2s
grace → KILL, registry-scoped measurement), the same memory→load sequence
completes for Ghostty with **zero leftover processes** (verified 2026-07-16:
load-active throughput 3890 ms CPU / scroll 1540 ms CPU in the sequence run;
3220/1530 ms in a load-only run — quick mode, so indicative not headline).
The fix removes the whole failure class rather than one re-triggerable
symptom; the decisive full-suite run should confirm all four terminals have
complete load-active data.

## Equalized re-run (`bench/run_all.sh --equalize`)

A second mode pins every terminal to the **same grid (120×40) and font
(Menlo 14pt, no ligatures / no background effects)** and re-runs the two
render-sensitive axes (throughput + scroll). Latency/startup/memory/load are
skipped in this mode — the first two are condition-independent, and
memory/load equalization (grid size does affect GPU buffer sizing) was out of
scope for this pass; see `run_all.sh` for where to add it if needed. What
could and couldn't be equalized:

| Terminal | grid 120×40 | font Menlo 14 | how |
|---|---|---|---|
| noa | ✅ `--cols/--rows` | ✅ config swap (also strips ligatures + bg-image/blur) | CLI + temp config |
| Ghostty | ✅ `--window-width/height` (grid cells) | ✅ `--font-family/--font-size` | CLI, `--config-default-files=false` |
| kitty | ✅ `-o initial_window_width=120c …` | ✅ `-o font_family/font_size` | `--config NONE` + `-o` |
| **Termy** | ❌ **no size key exists** (CLI or config) | ✅ `font_family/font_size` in config | config swap only |

Termy's window/grid size is **not equalizable** — it has no CLI size flag and
no columns/rows/window key in `config.txt`, so it runs at its native default
grid. Recorded as `grid=NATIVE-DEFAULT` in `results.json.equalized`. Since
2026-07-16 the equalized noa/Termy configs are written into the per-run
**isolated** config dir (see "Config isolation") — the user's real config
files are never touched (the old backup/swap/restore machinery is gone).

**Result:** throughput was unchanged from baseline for every terminal
(≤3% drift) — confirming throughput is grid/font-independent (flow-control- and
parse-bound). Scroll ranking was also unchanged (noa < Termy < kitty < Ghostty).

## Ghostty throughput/scroll anomaly — investigated, it is REAL

Ghostty 1.3.1 measured much slower than its reputation (≈70 MiB/s ASCII,
≈945 ms scroll). Verified it is **not a config artifact**: the machine's Ghostty
config only sets a theme, Fira Code + ligatures (`+calt/+liga`), and
`adjust-cell-height`. An A/B with `--config-default-files=false` (clean defaults)
gave ~72–75 MiB/s ASCII and ~945–999 ms scroll — within noise of the
user-config numbers. Ligatures don't affect the flow-control-bounded consume
metric (shaping is render-side and cached). So Ghostty's low numbers on **this
metric / this machine** are representative, not misconfiguration.

## Contention sensitivity (important for the render axes)

The render/scroll axis exposed a real architectural split, visible when the
machine is under CPU load (e.g. concurrent `cargo`/`rustc`):

- **noa and Ghostty are CPU-bound on consume** — they drain the pty as fast as
  the CPU allows. Fast when CPU is free, but their scroll times balloon and
  become high-variance under contention (noa 169 ms quiet → 500+ ms loaded;
  Ghostty 945 → 2000 ms).
- **Termy and kitty are display-paced** — they drain at roughly display cadence,
  so their scroll time is nearly constant regardless of CPU load (Termy ~265 ms,
  kitty ~367 ms, min≈median in every run).

Consequence: on a contended machine the *median* of a few scroll reps
understates noa/Ghostty. The harness reports medians; for the render axes on a
shared machine, take the **minimum of N reps** (least-contended ≈ true
capability) — noa's contention-robust equalized scroll is ~169–200 ms
(#1), Ghostty ~945 ms (#4). This CPU-bound-vs-display-paced distinction is itself
a finding worth stating alongside any headline scroll number.

### Builder-quiescence gate + contention bookends (2026-07-16)

Because of the sensitivity above, the harness now **enforces** the quiet-
machine requirement instead of assuming it:

- **Gate:** before doing anything, a read-only `ps -axo pid,comm` scan
  checks for live `cargo` / `rustc` / `clang` / `clang++` processes
  system-wide. If any are found the run **refuses to start** with a clear
  message listing them (`exit 3`). `--force` overrides the gate; a forced
  run records `quiescence_check = "FORCED-PAST-LIVE-BUILDERS: <pid:name…>"`
  so the contamination is permanently attached to the results. The scan
  never kills or signals anything.
- **Bookends:** `loadavg` (`sysctl vm.loadavg`) and `uptime` are recorded at
  run start *and* end, plus a second builder scan at the end
  (`builders_at_end`) — a build that started mid-run is caught too. All of
  it lands in `results.json.contention` and is echoed at the top of
  `table.md`, so any results file shows at a glance whether the machine was
  actually quiet while it was produced.

## Equalization notes & limitations

- **Grid size is not fully equalized.** noa is pinned to 120×40; the other
  terminals open at their own default window/grid size (no portable per-terminal
  cell-count flag). Throughput is dominated by parse cost, which is largely
  grid-size-independent; scroll is mildly grid-sensitive. Documented, not hidden.
- **Font is each terminal's default.** Not equalized; affects render axes
  slightly. A future pass could pin a common font via each terminal's config.
- The DSR proxy excludes display/input hardware latency (see axis 2).
- Throughput completion is flow-control-bounded, not last-glyph-painted (axis 1).
- GUI windows open and close on the user's display during the run; this is
  expected. Harness-spawned processes (and only those — see the PID-scoped
  lifecycle section) are killed between runs, at the end, and by an EXIT trap
  on abort.
- The launch *verb* differs per terminal and shows up in the multitab
  `proc_breakdown`: Ghostty's `-e` wraps the command in `login`
  (4 procs/window since the 2026-07-16 phantom-window fix; the previously
  observed 6 included a phantom `login → zsh` restored-state subtree, see
  that section), kitty adds a `kitten` helper per window (4 procs/window),
  noa/Termy run the pty child directly (3 procs/window). Each terminal's own
  wrapping is part of its own measured cost; the pty child itself is
  identical everywhere.
- Numbers are comparable **within one results directory / one machine**; do not
  compare across machines or macOS versions.

## Reproducing

```bash
cargo build --release -p noa --offline   # or without --offline outside sandbox
bench/run_all.sh                         # full 6-axis suite
bench/run_all.sh --quick                 # smoke (fewer reps/shorter settles)
bench/run_all.sh --only noa,kitty        # subset of terminals
bench/run_all.sh --axes memory,load      # subset of axes
bench/run_all.sh --force                 # bypass the builder-quiescence gate
```

A run refuses to start while `cargo`/`rustc`/`clang` are alive anywhere on
the machine (see "Builder-quiescence gate"); finish the build or pass
`--force` (the override is recorded in the results).

Outputs: `bench/results/<timestamp>/{results.json,table.md,raw.tsv,METHODOLOGY.md}`.
`raw.tsv` retains every per-rep sample for independent re-aggregation.

## Claim scoping (final)

- **Startup**: noa is #1 on *usable* startup (window visible AND prompt ready: 143ms vs Ghostty 198 / Termy 211) and on pty-ready (66ms). On *blank-frame-visible* alone, Ghostty wins (42ms vs noa 143ms) — noa boots the shell first, Ghostty paints first. Both readings are reported; pick the definition that matches your question.
- **Input latency**: measured as DSR (ESC[6n) round-trip = pty→parser→responder loop, the same path as keystroke echo. It is a *parser-respond* proxy, not keyboard-to-glass photon latency. noa additionally ships `NOA_LATENCY_TRACE=1` for internal key→present timing (~1.6ms median), but no cross-terminal keypress-to-glass comparison exists.
- **Scope**: all numbers are from one Apple M4 (Mac16,13), macOS 26.5.1, single day, terminal versions as recorded in each results dir. Rankings are machine-scoped; rerun `bench/run_all.sh` to reproduce on your hardware.
