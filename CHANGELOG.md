# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- Kitty temporary-file transfers require both an approved temporary directory
  and the protocol filename marker. Shared-memory transfers validate the whole
  requested range and copy through the kernel; `S` is a byte count independent
  of `O`. Failed placements still enforce the image storage quota.
- PNG decoding expands packed grayscale, palette colors and transparency, and
  checks decoded size before allocating pixel buffers. GPU uploads respect the
  device dimension limit; deleting and recreating an image cannot reuse stale
  texture contents.
- PTY exit waits for the reader's final output, with a two-second drain deadline
  for descendants retaining the slave. User input and terminal replies share a
  nonblocking byte budget; shutdown cancels writes waiting for PTY capacity.
- Search finds text across soft wraps and highlights every matching row. CSI
  requests exceeding the parameter limit are ignored without changing accepted
  parameter values. Panes sharing a redraw deadline emit one redraw notification.

### Changed

- Search runs on immutable snapshots in a worker with 35 ms debounce and
  cancellation of superseded queries, moving history scans outside the UI
  thread and terminal lock.
- Image storage uses ID/number/age indexes and caps image/frame metadata counts.
  Static image placements reuse GPU uniform buffers and bind groups.

## [0.2.9] - 2026-09-05

### Fixed

- Claude Code installed through npm is recognized as an agent. The tty's
  foreground process for an npm install is the `node` wrapper, and the
  wrapper's argv was only inspected for Codex, so Claude Code panes showed
  "node", BEL never promoted to attention, and the auto-approve fire gate
  rejected every candidate. Wrapper canonicalization now covers Claude Code as
  well, including the common `bin/claude` symlink launch whose argv carries no
  package name, and an explicit npm package reference takes precedence over
  the executable-basename heuristic so a Claude Code launch mentioning a
  `codex` path is not branded Codex (#62)
- Raw C1 bytes in ground state decode as invalid UTF-8 instead of opening a
  control string, so one stray byte from damaged multi-byte text can no longer
  swallow the visible output that follows (#61)
- Colon-form SGR subparameter groups stop at the first semicolon and no longer
  borrow the following attribute (#61)
- DECOM origin-relative cursor addressing is applied to CUP/CHA/VPA, margin
  homing, CPR, and saved-cursor replay; IRM insert mode shifts existing cells;
  ICH/DCH/IL/DL/SU/SD and TAB/CBT are confined to DECSLRM margins; images
  outside the margins survive rectangle scrolls (#61)
- Shrinking the row count keeps populated rows below the cursor and counts
  erased rows as disposable (#61)
- `noa.getText` IPC responses are capped to the WebSocket write buffer so a
  large read cannot disconnect the control client (#61)

## [0.2.8] - 2026-08-24

### Fixed

- Modifier keys remain synchronized with the native window that originated
  each input event. Queued events, native-tab switches, occlusion changes, and
  focus transitions could previously make the shared modifier cache diverge
  from winit's per-view state, intermittently turning Shift+Arrow into a plain
  Arrow or a normal key into a Cmd shortcut. Modifier snapshots are now kept
  per window and loaded before keyboard, pointer, and Overview dispatch, while
  unrelated windows can no longer clear or overwrite the active input state
  (#59)

## [0.2.7] - 2026-08-24

### Fixed

- Shell integration survives the OS temp reaper. The integration scripts were
  materialized once into `$TMPDIR/noa-shell-integration-<pid>` and the path
  then cached in a `OnceLock` and handed out unchecked forever, so once that
  tree was removed under a running noa every later pane still got the stale
  path — and because `ZDOTDIR` (zsh) and `--rcfile` (bash) suppress the
  shell's normal startup lookup, the shell came up with no configuration at
  all, not even the user's own. The scripts are re-verified on every handout
  and rewritten when any went missing, so an unusable directory degrades to
  "no integration" rather than to a dangling path, and materialization failure
  is no longer cached. Per-launch directories left behind by earlier runs are
  swept as well (#57)

## [0.2.6] - 2026-07-31

### Added

- Scrollback persistence: with `scrollback-persist = tail` each pane's
  scrollback tail is captured on clean quit and on idle checkpoints, then
  restored above the live shell on the next launch — separated by a labeled
  boundary row and marked with a gutter, so recovered history can never be
  read as this session's output. Restored rows are ordinary scrollback, so
  selection, copy and search work on them unchanged. Off by default
  (`never`, matching Ghostty, which restores window topology but never
  terminal contents); `scrollback-persist-limit`,
  `scrollback-persist-total-limit` and `scrollback-persist-max-age-days`
  bound per-pane bytes, total store size and record age. The on-disk NOASB
  format works at the materialized row level rather than serializing the
  paged scrollback, because grapheme and hyperlink ids are process-scoped
  and would decode to different text in the next process; restore rewraps
  to the current grid width, so a snapshot taken in a narrow window does not
  come back with its soft-wraps frozen (#52)
- bench: latency-under-load axis (`latload`) measuring DSR round-trip while
  the terminal is under a heavy write flood, with a `bulk_produce` helper,
  a wrapper mode, `run_all.sh` integration, and its methodology written down
  in `bench/METHODOLOGY.md` (#53)
- bench: the repository's first font benchmarks — `bench_size_change`
  (font-stack discovery, `FontGrid` construction and prewarm, split by
  stage) and `bench_atlas_sync` (the GPU half: atlas texture recreation,
  full re-upload and bind-group rebuild). Together they quantify what a font
  size or DPI change actually costs, which nothing in the repo had measured
  before (#55)

### Changed

- A font size or DPI change costs ~58% less main-thread time (63 → 27 ms per
  step on an M-series Mac, 3.8 → 1.6 frames of stall at 60 Hz). Nerd Font
  family-name resolution walked every installed family through CoreText on
  every rebuild — ~16 ms, 78% of a whole font-stack load — despite depending
  on neither the config nor the pixel size; it is resolved once per process
  now. Declared trade-off: a Nerd Font installed while noa is running is not
  picked up until restart, bounded by Symbols Nerd Font Mono shipping inside
  the binary (#55)
- Each window now uses the font grid for its own scale factor instead of one
  app-wide grid rebuilt at whichever window last reported a change, so on a
  mixed-DPI setup windows no longer rasterize at a size that is not theirs.
  Grids are kept in a live map keyed by pixel size (bounded at 6 per role,
  LRU-evicted, never the primary), so a size already visited comes back
  instead of being rebuilt — 14 → 15 → 14, or dragging a window between a 1x
  and a 2x display, re-rasterizes nothing. Glyph atlases are keyed by
  `(format, ppem)` to match (#55)
- Frame snapshots reuse clean rows across pure vertical scroll instead of
  rebuilding from scratch: the recycle key included the row base, so a
  scrolling viewport (a `cat` flood, build logs) invalidated it every frame
  even though nearly every visible row was unchanged content that had merely
  moved. When the key mismatches only by row base and the viewport was
  auto-following in both frames, the recycled row buffer is rotated by the
  delta before the ordinary clean/dirty pass. Per-call
  `FrameSnapshot::from_terminal_recycle` on a pure-scroll flood drops from a
  ~3.3–5.3 µs median to ~0.5–0.9 µs (roughly 6–8x); pinned or
  history-scrolled viewports, and partial (DECSTBM) scroll regions, still
  fall back to a full rebuild (#54)
- Glassmorphism: the `glassmorphism` key now takes a 5-step level (`off` and
  `1`–`5`, higher = more transparent) instead of a plain on/off flag. `1` is
  byte-identical to what `true` has always resolved to (0.50 window opacity /
  64 blur radius); `2`–`5` push further (0.35 / 0.20 / 0.12 / 0.06) for more
  of the desktop to show through, with the chrome alphas (sidebar/overview
  backdrop, surface, pill, and the shared overlay cards) stepping down to
  match and the rim brightening to carry the edge a thinner face no longer
  draws — at `5` it reaches the foreground color, which is where the ladder
  stops. Existing `glassmorphism = true`/`false` configs keep working and
  resolve to `1`/`off` respectively; reading a config written for the old key
  is unaffected. Note that the *written* spelling changes: the Settings panel
  and `noa --config` now emit `off`/`1`…`5`, so saving or reverting from the
  panel rewrites a hand-written `glassmorphism = true` as `glassmorphism = 1`.
  The panel's Glassmorphism row cycles through all six steps instead of
  flipping a toggle

### Fixed

- A pane restored with no scrollback record now says so, in a row naming the
  key that would change it, instead of coming back silently empty. Restoring
  the tabs and splits is itself a promise an empty pane breaks, and the
  previous behavior left it ambiguous whether the output had been lost or
  merely never kept. Ships regardless of whether persistence is enabled (#52)

## [0.2.5] - 2026-07-27

### Added

- Glassmorphism: the `glassmorphism` key renders noa's own chrome — the session
  sidebar, the tab overview, the modal cards — as frosted glass over a
  see-through window. Off by default. When on it takes `background-opacity` and
  `background-blur-radius` over outright rather than composing with them (a
  frosted panel over an opaque window is a no-op), and hands the configured pair
  back when turned off again. Exposed in the Settings panel and applied live,
  both on a config reload and on a toggle committed from the panel (#49)

### Changed

- Sidebar session cards render their relative timestamps and the attention label
  in English; they were the last Japanese strings left in the UI (#49)

### Fixed

- Oversized fallback glyphs are fit to their cell span instead of overlapping
  the neighboring cell. Circled digits (U+2460+, East-Asian-Ambiguous, width 1)
  resolve to macOS fallback faces whose glyphs advance ~2 cells; the
  fit-to-cell shrink existed but was gated to Nerd Font icon faces for Ghostty
  parity. A deliberate deviation from Ghostty in favor of readability (#48)

## [0.2.4] - 2026-07-24

### Added

- Scratch terminal: a disposable centered popup shell (cmd+shift+t) that spawns
  in the focused pane's OSC 7 cwd and is destroyed on toggle, focus loss, shell
  exit, cmd+w, config reload, or quit. Discoverable via the View menu and
  Settings; marked ephemeral with an accent ring and a persistent
  "Scratch Terminal — <cwd>" badge. Config: `scratch-terminal-key`,
  `scratch-terminal-size` (#45)

### Changed

- Legacy Japanese documentation (KEYBINDINGS, parity plan/README, feature
  specs) translated to English (#45)

### Fixed

- Native tab reordering no longer breaks the window↔tab-item association:
  AppKit auto-appends new native tabs before Noa repositions them, so an
  existing member is now detached before reinsertion, keeping window order and
  tab labels in sync (#46)

## [0.2.3] - 2026-07-23

### Added

- Sidebar session cards report categorical shell activity (via OSC 133) through
  status rails and surface task progress (determinate, indeterminate, paused,
  and error states) on cards and in the Tab Overview, with repeated attention
  blinking replaced by a bounded one-shot emphasis (#42)

### Changed

- Full pane rebuilds (e.g. tab-switch reveals that miss the warm-cache fast
  path) memoize per-(char, style) font/glyph resolution instead of rescanning
  the whole font stack per cell: warm-atlas full rebuild 38ms -> 14ms (-63%),
  cold-atlas 76ms -> 53ms (-30%) (#43)

### Fixed

- Native tab labels no longer regress to stale cached values after a relayout
  (pane split/close, font size change, sidebar toggle, fullscreen): AppKit
  re-derives tab labels from window titles on tab-group layout passes, so a
  debounced all-window title re-assert now runs once the relayout burst goes
  quiet (#41)

## [0.2.2] - 2026-07-22

### Added

- File paths in terminal output (absolute, `~/`, `./`, `../`, and bare
  relative tokens, with rustc/grep-style `:LINE[:COL]` suffixes) become
  Cmd+click links on local panes: hover resolves the text against the
  pane's cwd and linkifies only if the target exists on disk, probed on a
  worker thread through a TTL'd cache so a wedged network volume never
  stalls rendering (#38)
- Panes can be rearranged and moved across tabs by dragging them in the
  Tab Overview, which now renders each tab as a layout minimap compositing
  its panes at their split-tree positions: dragging within a tile swaps or
  split-inserts (center/edge zones), dropping onto another tab's tile
  moves the pane there with position targeting, and the running process is
  carried alive across the move (#39)

## [0.2.1] - 2026-07-21

### Fixed

- Shell- and tool-driven OSC 0/2 titles (Claude Code task names, ssh, tmux,
  REPLs) win over the dynamic process/cwd title again; staleness is judged by
  a cwd fingerprint captured when the title is set, so stale startup titles
  still fall back to the dynamic title. The rebind window is
  hook-order-independent and closes at OSC 133;A / 133;C or an executed
  LF/CR, and the XTWINOPS title stack (CSI 22/23) saves and restores the
  fingerprint alongside the title (#36)
- Closing the focused native tab no longer leaves the shared titlebar
  showing the closed tab's title: the promoted window's applied-title mirror
  is cleared so the next refresh re-asserts unconditionally (#36)
- Occluded native tabs keep their labels fresh: the lightweight title
  re-assert is decoupled from the throttled background pane-cache refresh,
  so idle or occlusion-flapped background tabs no longer freeze at an old
  title (#36)
- Tab-bar buttons repaint on every title change: the resolved title is now
  explicitly mirrored onto the NSWindowTab, so a spawn-inherited startup
  title no longer bakes into the button while the titlebar moves on (#36)

### Changed

- Local tab titles hide the `user@host:` prefix emitted by shell title
  hooks when the user and short hostname match the local machine; remote
  sessions (ssh) keep the full identity (#36)

## [0.2.0] - 2026-07-21

### Fixed

- Local tab titles now keep an explicitly assigned title authoritative;
  without one, they prefer the live foreground process or current working
  directory and use OSC 0/2 titles only as a fallback, preventing shell
  startup titles from pinning the tab to its launch directory (#34)

## [0.1.9] - 2026-07-20

### Added

- bench: fixed-168x36 region mode for the fire benchmark, enabling
  geometry-independent runs that approximate fullscreen sizes (scored
  upstream DOOM-fire runs still default to 80x24)

### Changed

- Release binaries are PGO-optimized: profiles are collected headlessly via
  the noa-grid ingest benches, merged with llvm-profdata, and the release
  bundle rebuilds with `-Cprofile-use` (+6% ascii / +4% synthetic ingest
  throughput on M4; `NOA_PGO=0` opts out) (#30)
- DSR/DA report replies flush per parsed chunk instead of at the drain-batch
  tail, so a loaded latency probe's round-trip is no longer bounded by the
  remaining parse time of an up-to-1MiB batch (#33)
- bench: fire producer v2 overlaps frame compose with the pty write
  (two-buffer ping-pong; identical byte stream, but v1/v2 fps are not
  comparable), and fire results rank by geometry-fair Mcells/s instead of
  raw fps; raw.tsv records the producer version so aggregations refuse to
  mix v1/v2 (#31)

### Fixed

- Background-tab window titles no longer freeze at their last-foreground
  value: title resolution moved ahead of the occlusion early-return, so
  closing a tab promotes the next tab with its correct title (#32)

## [0.1.8] - 2026-07-19

### Added

- bench: the fire axis, multi-terminal support (Alacritty, iTerm2, Warp,
  Terminal.app, Rio), fullscreen render-path measurement, HTML reports, and
  `docs/positioning.md` first ship in this tag — they were described in the
  0.1.7 notes but merged after the v0.1.7 tag was cut (#23)
- bench: `ghostty-nightly` terminal entry for a frozen-build baseline, a
  headless `cell_bandwidth` probe (consume/frames/store), the in-tree
  drain-staircase probe harness (S1 read / S2 parse / S3–S4 apply lenses),
  and a single-run `--real-one` mode for pipeline iteration

### Changed

- PTY drain ingest overhaul: complete plain and edge-SGR styled line floods
  are now recognized in the VT ground scan and applied as amortized line
  batches (`print_ascii_lines` / `print_sgr_ascii_lines`) with one grid
  rotation per batch; the screen grid became a base-offset ring so the
  full-height LF scroll is an O(1) base bump instead of an O(rows) rotate;
  batched rows seal to scrollback as raw byte spans packed straight to
  pages, skipping Cell-row materialization. ansi staircase real S3
  326→416 MB/s, proc S4 480→661 MB/s (#29)
- PTY reader: the master is O_NONBLOCK and drains the whole tty queue per
  wakeup; the refill bridge spins instead of yielding mid-flood and the
  reader declares USER_INTERACTIVE QoS, keeping the kernel tty queue from
  brimming while pipeline threads are runnable (S4 plain 292→360 MB/s) (#29)
- Scrollback sealing is truly asynchronous under sustained floods: seal
  batches scale with width to a ~1MiB raw-byte target (capped by the
  over-limit estimate allowance), a row-occupancy watermark skips
  blank-tail loads, and the packed store emits one u64 per cell with
  immediate span pack — feed_bench ascii 410→~500 MiB/s, in-app `time cat`
  0.472→0.455s (parity with ghostty nightly's 0.454s)
- `Cell` redesigned as a 24-byte POD with interned grapheme tails
- VT parser: whole in-chunk CSI sequences bypass the per-byte DFA, ground
  state C0 controls dispatch without it, pre-verified ASCII runs skip
  `print_str`'s re-scan, and the fire-shape micro path adds stack SGR
  dispatch plus a narrow print fast path

## [0.1.7] - 2026-07-17

### Added

- Dynamic tab titles: when the shell has not set an OSC 0/2 title and the tab
  has not been renamed, the title is derived live from the focused pane's
  foreground process and OSC 7 cwd tail (`cargo — noa`), matching the sidebar
  card naming; a plain interactive shell collapses to just the cwd tail (#22)
- bench: seventh harness axis "fire" (DOOM-fire IO stress, deterministic
  truecolor stream, producer-side fps), native-fullscreen measurement for all
  render-path axes on the launch display, five more terminals (Alacritty,
  iTerm2, Warp, Terminal.app, Rio — activate only when installed), and
  self-contained HTML reports; `docs/positioning.md` states the
  output-flood-first positioning (#23)

### Changed

- Keyboard/paste/IME input now bypasses the io thread's output-batch loop and
  writes straight to the pty writer thread, with the three keyboard-encoding
  modes read under a single terminal lock and echo-repaint debt tracked as a
  write-generation counter: loaded key-to-echo latency no longer degrades
  behind 1MiB output batches (#21)
- Switching to a tab with heavy output no longer stalls for a frame: occluded
  windows keep their pane cache warm via a globally throttled (250ms)
  background refresh, and the first frame after reveal presents the cached
  instances instantly, deferring the incremental rebuild to an immediately
  scheduled follow-up frame (was ~93ms cold / ~38ms warm full rebuild at
  200x60) (#24)

### Fixed

- `sidebar-hotkey` is no longer registered as a system-wide Carbon global
  hotkey (it grabbed cmd+shift+s — "Save As…" — from every application); it
  is now an in-app rebind of `ToggleSidebar` through the keybind engine, with
  explicit `keybind` entries still winning, conflict diagnostics, and the
  Sidebar menu accelerator tracking the effective chord (#25)
- Closing a native macOS tab left the surviving tab unable to receive
  keyboard input: AppKit moves first responder off winit's content view
  during tab teardown; the deferred focus-restore path now reassigns first
  responder to the content view and re-arms IME (#26)
- After closing a tab with cmd+w, the next plain keypress could resolve as a
  chord (e.g. `f` opening Search): stored modifiers are now reset on focus
  loss, and the synchronous focus-restore block that raced AppKit teardown
  (intermittent crash) is removed in favor of the deferred path (#27)

## [0.1.6] - 2026-07-16

### Changed

- Idle memory: the ~18 curated CJK fallback fonts are no longer resolved
  eagerly at startup (which faulted whole `.ttc` files into the page cache);
  they resolve lazily on the first CJK glyph miss via the same
  priority-ordered, cmap-gated lookup, so glyph selection is unchanged.
  Measured idle RSS: 239MB → 133MB (#20)
- Shell integration prompt marks (OSC 133 D/A/B + OSC 7) are now emitted as a
  single builtin `printf` per prompt, with OSC 7 switched to the
  `kitty-shell-cwd://` scheme (raw path, no per-character percent-encoding):
  ~60µs → ~7µs per prompt in zsh, ~3.7ms → ~7µs on non-ASCII paths (#19)

### Fixed

- Shell integration no longer leaks noa-only bookkeeping into the session
  environment: `ZDOTDIR` is unset after startup when the user never had one
  (Ghostty parity), and `USER_ZDOTDIR` is a plain unexported variable (#19)

## [0.1.5] - 2026-07-16

### Added

- Remote App QR pairing: a settings-panel row renders the noa-server
  connection payload (URL + token) as a QR code for one-scan pairing from a
  remote app (#15)
- Client mode: attach a pane to a remote noa-server as a raw VT stream, with
  parser-state seeding, scrollback history backfill, and client connection
  config keys (#12)
- Keyboard-only copy mode: grid-owned selection with character/word/line
  motions, rendered selection and hollow cursor, plus keybindings and command
  palette entries (#11)
- `send-selection-send-enter` config key (default off): the send-selection
  picker follows the paste with an Enter, queued atomically with the paste so
  a dropped paste never submits a stale prompt line (#10)
- `-e <command...>` CLI flag: run a command instead of the login shell in the
  first window (Ghostty `initial-command` parity — first surface only;
  suppresses session restore)
- `bench/`: reproducible 4-axis cross-terminal benchmark harness
  (`bench/run_all.sh` — throughput, scroll, DSR latency, dual-sentinel
  startup) with methodology and recorded results
- Env-gated performance instrumentation: `NOA_LATENCY_TRACE=1`
  (key→present timing) and `NOA_STARTUP_TRACE=1` (startup stage breakdown)
- `cursor-stop-blinking-after` config key (default `10` seconds): the cursor
  settles solid after that long with no input/output on the focused pane, so
  an idle noa schedules no blink wake-ups. **Intentional default deviation
  from Ghostty** (which blinks forever), benchmark/idle-power motivated —
  `0` restores Ghostty-parity eternal blink (see CONFIGURATION.md
  "Deviations from Ghostty defaults")

### Changed

- New tabs are inserted after the current tab instead of at the end, with
  native and internal tab order kept aligned for navigation, closing, and
  session persistence (#14)
- Config live-reload cadence: the idle file poll slowed from 500ms to 3s;
  window focus gain and settings-panel commits now expedite an immediate
  check instead. Net effect: edits made in another app apply on refocus
  (faster than before), while a save from *inside a focused noa pane* (e.g.
  `vim` editing the config in that window) applies within ≤3s (was ≤500ms) —
  refocus the window or use the settings UI to apply instantly
- Query-only pty batches (DSR/DA/DECRQM/XTVERSION/Kitty-keyboard reports —
  e.g. a TUI capability poll or latency probe) no longer wake the renderer:
  nothing visible changed, and skipping the snapshot pass removes the main
  contributor to the DSR round-trip p99 tail

- Bulk-output throughput: scrollback rows are now sealed in deferred batches
  and packed off-thread; pty reads are flow-controlled by a byte budget with
  congestion read-coalescing (ASCII +22%, Unicode +38% on the reference M4)
- Input latency: swapchain depth lowered to 1, keystroke echo bypasses the
  redraw floor, and pipeline threads use a traffic-gated bounded spin before
  parking (DSR round-trip 16µs median / 51µs p99 on the reference M4)
- Warm startup: the pty is pre-spawned, the primary font face loads before
  the full fallback stack, the GPU is prewarmed, and the window shows with a
  pre-painted theme-background frame before font/renderer init completes
  (window-visible + prompt-ready in ~143ms on the reference M4)
- Unicode print path: SIMD UTF-8 validation, BMP-indexed width table, and
  unified decode (no re-decoding between parser and grid)

### Fixed

- Child processes now see `TERM_PROGRAM=Noa`, so shells and TUIs can identify
  the hosting terminal (#17)
- `noa-ipc` grid coordinates are stable session-absolute row coordinates:
  `getGrid` and output notifications survive scrollback eviction, coordinate
  generations are versioned, and copy-mode indexing is translated once at the
  IPC boundary (#13)
- Keystroke echo could be delayed up to one redraw-floor interval (~8ms)
  while cursor-blink repaints were active
- IME composition (`Preedit`/`Commit`) now counts as typing for
  `cursor-stop-blinking-after`: a CJK composition paused longer than the
  idle window no longer freezes the cursor solid mid-preedit
- `noa-ipc`: a server shutdown whose loopback wake connection failed used to
  leak the accept thread parked in `accept()` forever; it now force-closes
  the listening socket (fd-reuse-safe dup2-over) and joins the thread with a
  bounded timeout

## [0.1.4] - 2026-07-13

### Fixed

- Closing a native tab no longer leaves the newly selected tab unable to
  receive input: focus is restored after AppKit finishes its own tab
  selection instead of racing it

## [0.1.3] - 2026-07-13

### Added

- `noa-server`: JSON-RPC over WebSocket control server (new `noa-ipc` crate)
  with token auth, read/input/manage scopes, pane output subscriptions,
  configurable bind address for LAN access, and settings-panel rows for
  enable/port/scopes/bind, server status, and one-click token copy (#2)
- Process monitor overlay listing per-pane foreground process, CPU, and
  memory, backed by foreground-process-tree metrics collection (#2)
- JPEG and WebP background images (magic-byte dispatch, decode capped at
  64 MiB RGBA), including slideshow support (#4)
- Configurable sidebar width (`sidebar-width`, 200-600 pt) and font size
  (`sidebar-font-size`, 8-20 pt) with live Settings rows (#9)
- Embedded Symbols Nerd Font Mono fallback so Nerd Font private-use-area
  icons render without a locally installed Nerd Font (#8)

### Changed

- Hot-path performance: combining-buffer and row-instance-buffer reuse,
  cached cursor blink state, terminal lock split across PTY chunk
  boundaries, linear-scan mode storage, SWAR printable-run scanning, and
  in-place cell erase/shift (#5)
- Documentation (specs, user guide, runbooks, protocol references,
  benchmark README) translated to English

### Fixed

- Cmd+K clear repaints the shell prompt instead of leaving a blank screen,
  matching Ghostty's clear semantics (prompt-aware via OSC 133, no-op on
  the alternate screen) (#7)
- Fallback glyph styling and sizing aligned with Ghostty: no synthetic
  bold/italic on fallback faces, natural-size rasterization for ordinary
  text, cell-fit only for Nerd Font icons (#6)
- IME preedit and OS composition are discarded on window focus loss, so
  refocusing no longer swallows keypresses (#3)
- Redraws during synchronized output reuse the pre-sync snapshot (#5)

## [0.1.2] - 2026-07-11

### Added

- Theme & Settings overlay v2: favorites, attribute filtering (cycle with Tab,
  hop back with Shift+Tab), undo toast, and mouse-wheel scrolling, with the
  overlay split into dedicated Theme and Settings modes
- Settings panel enrichment: search, category badges, per-key descriptions,
  and reset-to-default, plus newly exposed `scrollback-limit`,
  `cursor-style-blink`, `minimum-contrast`, and `macos-option-as-alt` keys
- `Cmd+,` opens the settings overlay, and Tab reopens the last-used mode
- Mode-specific native macOS overlay view rendering with dedicated TUI text
  rendering for the theme settings overlay

### Changed

- Grid reflow is throttled during interactive resize
- Render and PTY locks use `parking_lot` mutexes, avoiding poison cascades
- Shape cache returns shaped runs as shared `Rc` slices; the VT parser reuses
  its SGR attribute buffer and pre-seeds OSC collection capacity
- Theme catalog data is `Arc`-shared, idempotent ViewModel rebuilds are
  skipped, and fuzzy rescans are narrowed

### Fixed

- PTY spawn failures are surfaced instead of silently closing, and io threads
  are reaped off the main thread
- OSC 52 clipboard writes are coalesced to the last write per feed batch
- Oversized kitty raw images are rejected before size arithmetic can overflow
- Light/dark pair theme configs are no longer silently overwritten
- Settings badge classification, font-family reset, no-op reset flash, and the
  scrollback-limit increase clamping bug
- Undo no longer reverts commit-only rows in the theme settings overlay
- Overview layout respects titlebar and content insets, and the overview
  search bar no longer hides under the tab bar
- Favorites chip no longer overlaps the cycle hint; overlay text widths are
  measured dynamically
- Command palette no longer shows a redundant Preferences item

## [0.1.1] - 2026-07-11

### Added

- AppleScript integration: sdef dictionary, Apple Event handler, app state
  snapshot, text input conversion, event-loop bridge, `macos-applescript`
  config key, and a smoke test script
- Kitty graphics: animation frames, shared-memory transfer, and a configurable
  image size limit
- Ghostty config compatibility: config-file includes, light/dark theme pairs,
  palette overrides, and the `block_hollow` cursor style
- Alpha-blending modes: `native` / `linear` / `linear-corrected`
- Quick terminal layouts, and appearance-driven theme switching with live
  palette reload
- Session overview paging so every session is reachable, with all pages live
- macOS titlebar proxy icon and force-click Quick Look
- Fallback glyphs are scaled to fit their cell span, preventing overshoot
- `NOA_PTY_CAPTURE` debug capture of raw PTY bytes
- Sidebar preview raised to a maximum of 20 lines

### Changed

- Scrollback rows are packed as style runs directly into the page arena
- Overview pill textures are cached and card GPU resources pooled
- Redraws are paced to the monitor refresh rate; idle kitty lock scans are
  skipped
- PTY writes no longer double-copy; the IME trace env check is cached
- io thread and sidebar band rendering split into focused modules, with new
  behavior tests and a cached-render-path equivalence test

### Fixed

- Quick terminal show/hide flicker, and quick terminal opening on a stale
  window's screen instead of the configured one
- Native overlay cards are kept alive across content syncs
- Overflowing kitty graphics geometry is rejected
- Total config-file includes are capped

## [0.1.0] - 2026-07-10

### Added

- Initial release: a faithful Rust clone of the Ghostty terminal emulator for
  macOS (Apple Silicon), built on `winit` + `wgpu`
- From-scratch VT parser (`noa-vt`) and terminal state model (`noa-grid`) with
  conformance tests
- GPU instanced-cell renderer, font discovery/rasterization/atlas pipeline,
  and the vendored Ghostty-compatible theme catalog (574 themes)
- Ghostty-compatible configuration discovery, parsing, and precedence
- Session sidebar, session overview, quick terminal, command palette, native
  tabs, and macOS app bundle packaging with signing/notarization CI

[0.1.6]: https://github.com/simota/noa/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/simota/noa/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/simota/noa/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/simota/noa/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/simota/noa/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/simota/noa/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/simota/noa/releases/tag/v0.1.0
