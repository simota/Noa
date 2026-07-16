# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

[0.1.5]: https://github.com/simota/noa/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/simota/noa/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/simota/noa/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/simota/noa/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/simota/noa/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/simota/noa/releases/tag/v0.1.0
