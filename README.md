![Noa](assets/noa-header.jpg)

# Noa

A faithful **Rust** clone of the [Ghostty](https://ghostty.org) terminal emulator — GPU-accelerated, macOS-first, built from scratch on `winit` + `wgpu`.

> Ghostty is written in Zig (Metal on macOS, GTK4/OpenGL on Linux). Noa reproduces its **observable behavior** — VT emulation, rendering, features — *idiomatically in Rust*. Today, fixture-based regression tests and observable parity probes guard that behavior; an automated Ghostty oracle for differential comparison is still planned.

## Status

**Increments 1-6 complete.** A GPU-accelerated terminal emulator written from scratch. The implementation features native multi-window/tab/split management, wgpu-based grid rendering with a Kitty-graphics image layer, PTY integration, CJK font fallback + ligatures, the Kitty keyboard protocol, paged byte-limited scrollback with interactive search, soft-wrap reflow on resize, shell integration (OSC 133/7), desktop notifications, session restore, a quick terminal, a command palette, background opacity/blur and background images, Ghostty-compatible `key = value` configuration parsing with live reload, and 574 vendored themes. Beyond the core parity increments it also adds a session sidebar, a session/tab overview, an agent auto-approve mode, and Ghostty config import (see [Roadmap](#roadmap)).

## Architecture

A Cargo workspace mirroring Ghostty's reusable-core / platform-apprt split. Lower-level crates remain windowing-agnostic; `noa-render` is the one GPU-facing lower-level crate, while `noa-app` owns windowing and application integration.

```
crates/
  noa-core      shared primitive types (Color, CellAttrs, geometry)
  noa-vt        from-scratch DEC ANSI VT parser + stream dispatch   ← fidelity core
  noa-grid      terminal state: screen grid, cursor, modes, scroll  ← fidelity core
  noa-font      glyph pipeline: font-kit discovery → swash raster → etagere atlas
  noa-theme     vendored Ghostty-compatible theme catalog (574 themes)
  noa-config    Ghostty-compatible config discovery / parsing / precedence
  noa-render    wgpu instanced-cell renderer (GPU-facing, not windowing)
  noa-pty       PTY spawn + reader/writer threads (portable-pty)
  noa-app       the apprt: winit event loop, Arc<Mutex<Terminal>>, io thread, input
bin/
  noa           thin binary → noa_app::run()
tests/
  parity        fixture-based regression harness (Ghostty oracle planned)
```

Dependency rule (enforceable via `cargo tree`): **only `noa-app` / `noa-render` may touch `wgpu`, and only `noa-app` may touch `winit`.** The VT parser and grid model have zero windowing dependencies so they stay unit-testable and reusable.

The **VT parser and terminal state model are written from scratch** (no `vte` / `alacritty_terminal`) — that is the fidelity core the whole clone is built to get right.

## Build & run

Requires Rust 1.92+ (edition 2024) and macOS (Apple Silicon).

```bash
cargo build --workspace     # build everything
cargo test  --workspace     # run the VT/grid conformance + smoke tests
cargo run   -p noa          # launch the terminal
```

Options: `cargo run -p noa -- --cols 100 --rows 30 --font-size 15`. Use
`--import-ghostty-config` to migrate supported settings from an existing
Ghostty config. One-shot queries use `+version`, `+list-themes`,
`+list-keybinds`, `+list-fonts`, `+show-config`, `+list-actions`, or `+help`.

### Configuration

At startup, Noa reads `config` from `$XDG_CONFIG_HOME/noa/config`
(`~/.config/noa/config` when `XDG_CONFIG_HOME` is unset). Missing config files keep
the built-in defaults: `window-width = 80`, `window-height = 24`,
`font-size = 14.0`, macOS `Menlo` as the default coding font with system CJK
fallbacks, `minimum-contrast = 1.0`, and the built-in terminal theme.
CLI flags override config file values.

The legacy `$XDG_CONFIG_HOME/noa/config.toml` path is detected only to emit a
migration warning; its TOML contents are not read. Move those settings to the
extensionless `config` file using the Ghostty-compatible `key = value` syntax.

```conf
window-width = 100
window-height = 30
font-size = 15.0
theme = "Catppuccin Mocha"
minimum-contrast = 3.0
confirm-quit = true
sidebar-preview-lines = 3
```

Theme names match the vendored Ghostty-compatible catalog in
`crates/noa-theme/vendor/themes/`, without the `.conf` suffix. For example,
`theme = "TokyoNight Night"`, `theme = "Gruvbox Dark"`, and
`theme = "Nord"` are valid. `--theme` is intentionally not a CLI flag; theme
selection is config-file only. `minimum-contrast` accepts a WCAG contrast-ratio
floor from `1.0` through `21.0`; `1.0` preserves theme colors unchanged.

### Build the macOS app

Noa runs as a proper foreground macOS app (Dock icon, custom native menu bar,
Cmd+Q/Cmd+W app shortcuts, native window controls). The menu bar shows `Noa`,
`File`, `Edit`, `View`, `Window`, and `Help`. The app menu includes `About Noa`,
`Settings…` (Cmd+`,`, opens the theme & settings overlay), `Secure Keyboard
Entry` (a checkable toggle), `Close Tab`, and `Quit Noa`. The `View` menu
exposes clear/clear-scrollback, font-size controls, `Session Overview`
(Cmd+Shift+O), `Command Palette` (Cmd+Shift+P), `Quick Terminal`, `Sidebar`
(Cmd+Shift+S), `Auto Approve`, full-screen toggle, and scrollback navigation —
line, page, top, and bottom scrolling via `Shift+ArrowUp/Down`,
`Shift+PageUp/PageDown`, and `Shift+Home/End`. To produce a double-clickable
`.app` bundle:

```bash
scripts/bundle-macos.sh          # → target/release/Noa.app  (ad-hoc signed)
open target/release/Noa.app      # or double-click it in Finder
```

The script builds a release binary, assembles the bundle (`Info.plist`, icon,
`PkgInfo`), and ad-hoc code-signs it so it launches without a Gatekeeper
prompt. The app icon is generated from scratch by `scripts/gen-icon.sh` using
`python3` + macOS `sips`; Python writes the ICNS container directly, so
`iconutil` is not required. `cargo bundle` also
works via the `[package.metadata.bundle]` in `bin/noa/Cargo.toml`.

## Fidelity approach

Noa follows a **fidelity-over-faith** discipline: compatibility claims should be measured rather than assumed. The current automated parity harness protects Noa's fixture outputs from regression; Ghostty-backed capture and oracle comparison remain future work. The Parity Map has five dimensions:

| Dimension | What "faithful" means | How it's checked |
|-----------|----------------------|------------------|
| **Behavioral** | Escape sequences, cursor/erase/scroll, deferred-wrap (xenl), DA/DSR replies behave identically | `noa-vt` / `noa-grid` unit tests (byte-sequence → action / grid assertions) |
| **Visual** | Layout, color, monospace metrics match per screen | side-by-side vs Ghostty running the same command |
| **Feature** | Every in-scope feature present & reachable | feature inventory coverage |
| **Data-shape** | Cursor clamps, grid semantics follow the DEC spec | unit tests |
| **Asset** | Fonts/glyphs faithful | system-font discovery + atlas |

### Increment-1 behavioral parity checks (feasible now)

- **Deferred wrap (xenl):** `printf 'a%.0s' {1..200}` wraps at the same columns as Ghostty.
- **SGR:** `printf '\e[31mRED\e[0m \e[1;32mBOLDGREEN\e[0m\n'` — 16-color + bold match.
- **Cursor report:** `printf '\e[6n'` returns a well-formed `ESC[row;colR`.
- **DA1:** the `ESC[c` probe gets `ESC[?62;4;22c` so the prompt doesn't hang.

## Roadmap

| Inc | Scope | Status |
|-----|-------|--------|
| **1** | Vertical slice: window + wgpu grid, PTY `$SHELL`, from-scratch parser (C0, core CSI, SGR 16+truecolor, deferred-wrap), block cursor, ASCII+arrow input, DA/DSR | ✅ Done |
| **2** | Resize behavior, full CSI/edit set, 256+truecolor palette, alt screen, DECSC/DECRC, bracketed paste, UTF-8 wide cells, interaction basics | ✅ Done |
| **3** | Paged scrollback storage, interned styles, OSC 8 hyperlinks, interactive search UI, expanded configuration | ✅ Done |
| **4** | Tabs + split tree + multi-window, config file, 574 themes, font fallback + ligatures + Nerd/box glyphs, soft-wrap reflow on resize | ✅ Done |
| **5** | Kitty graphics + keyboard protocols, shell integration (OSC 133/7), DCS, legacy mouse encodings | ✅ Done |
| **6** | macOS-native polish: quick terminal, command palette, background blur, session restore, secure keyboard entry, notifications (OSC 9/777), CLI actions (`+list-themes` …), titlebar styles, Option-as-Alt config | ✅ Done |
| **+** | Beyond core parity: session sidebar, session/tab overview, background images, agent auto-approve mode, Ghostty config import, live config reload | ✅ Done |

## License

MIT — matching Ghostty's license. Noa is an independent reimplementation; it links no Ghostty code.
