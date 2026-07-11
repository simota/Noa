# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

[0.1.1]: https://github.com/simota/noa/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/simota/noa/releases/tag/v0.1.0
