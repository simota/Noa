# Noa

A faithful **Rust** clone of the [Ghostty](https://ghostty.org) terminal emulator — GPU-accelerated, macOS-first, built from scratch on `winit` + `wgpu`.

> Ghostty is written in Zig (Metal on macOS, GTK4/OpenGL on Linux). Noa reproduces its **observable behavior** — VT emulation, rendering, features — *idiomatically in Rust*, verified against Ghostty by differential parity rather than by transliterating its internals.

## Status

**Increment 6 (In Progress).** A GPU-accelerated terminal emulator written from scratch. Currently, the implementation features native multi-window/tab/split management, wgpu-based grid rendering with a Kitty-graphics image layer, PTY integration, CJK font fallback + ligatures, the Kitty keyboard protocol, paged byte-limited scrollback with interactive search, shell integration (OSC 133/7), desktop notifications, session restore, a quick terminal, configuration file parsing (supporting both TOML and Ghostty formats), and over 460 vendored themes (see [Roadmap](#roadmap)).

## Architecture

A Cargo workspace mirroring Ghostty's reusable-core / platform-apprt split. Every crate below `noa-app` is GUI-agnostic (no `winit`/`wgpu`) — the same seam Ghostty draws between `libghostty` and its apprt.

```
crates/
  noa-core      shared primitive types (Color, CellAttrs, geometry)
  noa-vt        from-scratch DEC ANSI VT parser + stream dispatch   ← fidelity core
  noa-grid      terminal state: screen grid, cursor, modes, scroll  ← fidelity core
  noa-font      glyph pipeline: font-kit discovery → swash raster → etagere atlas
  noa-render    wgpu instanced-cell renderer (GPU-facing, not windowing)
  noa-pty       PTY spawn + reader/writer threads (portable-pty)
  noa-app       the apprt: winit event loop, Arc<Mutex<Terminal>>, io thread, input
bin/
  noa           thin binary → noa_app::run()
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

Options: `cargo run -p noa -- --cols 100 --rows 30 --font-size 15`.

### Configuration

At startup, Noa reads `config` from `$XDG_CONFIG_HOME/noa/config`
(`~/.config/noa/config` when `XDG_CONFIG_HOME` is unset). Missing config files keep
the built-in defaults: `window-width = 80`, `window-height = 24`,
`font-size = 14.0`, macOS `Menlo` as the default coding font with system CJK
fallbacks, `minimum-contrast = 1.0`, and the built-in terminal theme.
CLI flags override config file values.

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
`File`, `Edit`, `View`, `Window`, and `Help`. The app menu currently includes
`About Noa`, disabled `Preferences...` (shown as `Settings…` on current macOS),
`Close Window`, and `Quit Noa`; preferences and unsupported terminal actions
stay disabled until backing features exist. The `View` menu exposes scrollback
navigation: line, page, top, and bottom scrolling via `Shift+ArrowUp/Down`,
`Shift+PageUp/PageDown`, and `Shift+Home/End`. To produce a double-clickable
`.app` bundle:

```bash
scripts/bundle-macos.sh          # → target/release/noa.app  (ad-hoc signed)
open target/release/noa.app      # or double-click it in Finder
```

The script builds a release binary, assembles the bundle (`Info.plist`, icon,
`PkgInfo`), and ad-hoc code-signs it so it launches without a Gatekeeper
prompt. The app icon is generated from scratch by `scripts/gen-icon.sh` (pure
`python3` + macOS `sips`/`iconutil` — no external tools). `cargo bundle` also
works via the `[package.metadata.bundle]` in `bin/noa/Cargo.toml`.

## Fidelity approach

Noa follows a **fidelity-over-faith** discipline: the copy's match to Ghostty is *proven*, not asserted. The Parity Map has five dimensions:

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
- **DA1:** the `ESC[c` probe gets `ESC[?62;22c` so the prompt doesn't hang.

## Roadmap

| Inc | Scope | Status |
|-----|-------|--------|
| **1** | Vertical slice: window + wgpu grid, PTY `$SHELL`, from-scratch parser (C0, core CSI, SGR 16+truecolor, deferred-wrap), block cursor, ASCII+arrow input, DA/DSR | ✅ Done |
| **2** | Resize behavior, full CSI/edit set, 256+truecolor palette, alt screen, DECSC/DECRC, bracketed paste, UTF-8 wide cells, interaction basics | ✅ Done |
| **3** | Paged scrollback storage, interned styles, OSC 8 hyperlinks, interactive search UI, expanded configuration | ✅ Done |
| **4** | Tabs + split tree + multi-window, config file, ~460 themes, font fallback + ligatures + Nerd/box glyphs | ✅ Done |
| **5** | Kitty graphics + keyboard protocols, shell integration (OSC 133/7), DCS, legacy mouse encodings | ✅ Done |
| **6** | macOS-native polish: quick terminal, command palette, background blur, session restore, secure keyboard entry, notifications (OSC 9/777), CLI actions (`+list-themes` …), titlebar styles, Option-as-Alt config | ✅ Done |

## License

MIT — matching Ghostty's license. Noa is an independent reimplementation; it links no Ghostty code.
