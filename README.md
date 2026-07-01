# noa

A faithful **Rust** clone of the [Ghostty](https://ghostty.org) terminal emulator — GPU-accelerated, macOS-first, built from scratch on `winit` + `wgpu`.

> Ghostty is written in Zig (Metal on macOS, GTK4/OpenGL on Linux). `noa` reproduces its **observable behavior** — VT emulation, rendering, features — *idiomatically in Rust*, verified against Ghostty by differential parity rather than by transliterating its internals.

## Status

**Increment 1 — vertical slice.** A real, interactive terminal: a native window, a wgpu-rendered monospace grid, a PTY-backed `$SHELL`, a from-scratch VT parser, live colored output, and keyboard input. Later increments add resize/reflow, alt-screen, scrollback, tabs/splits, themes, Kitty protocols, and shell integration (see [Roadmap](#roadmap)).

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

## Fidelity approach

`noa` follows a **fidelity-over-faith** discipline: the copy's match to Ghostty is *proven*, not asserted. The Parity Map has five dimensions:

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

| Inc | Scope |
|-----|-------|
| **1** ✅ | Vertical slice: window + wgpu grid, PTY `$SHELL`, from-scratch parser (C0, core CSI, SGR 16+truecolor, deferred-wrap), block cursor, ASCII+arrow input, DA/DSR |
| **2** | Resize & reflow, full CSI/edit set, 256+truecolor palette, alt screen, DECSC/DECRC, bracketed paste, UTF-8 wide cells |
| **3** | Paged scrollback + reflow, interned styles, selection/clipboard (OSC 52), OSC 8 hyperlinks, keybind engine |
| **4** | Tabs + split tree, config file, ~460 themes, font fallback + ligatures + Nerd/box glyphs, mouse reporting |
| **5** | Kitty graphics + keyboard protocols, shell integration (OSC 133/7), DCS |
| **6** | macOS-native polish: quick terminal, command palette, background blur, session restore |

## License

MIT — matching Ghostty's license. `noa` is an independent reimplementation; it links no Ghostty code.
