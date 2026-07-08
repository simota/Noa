# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`noa` is a faithful **Rust clone of the [Ghostty](https://ghostty.org) terminal emulator** (Ghostty is Zig). It reproduces Ghostty's *observable behavior* idiomatically in Rust — it transliterates none of Ghostty's internals and links no Ghostty code. macOS-first (Apple Silicon), `winit` + `wgpu`. See `README.md` for the fidelity philosophy ("Parity Map") and the increment roadmap.

Requires Rust 1.92+ (edition 2024).

## Commands

```bash
cargo build --workspace          # build everything
cargo test  --workspace          # all VT/grid conformance + smoke tests
cargo clippy --workspace         # keep this clean — CI-equivalent gate
cargo run   -p noa               # launch the GUI terminal
cargo run   -p noa -- --cols 100 --rows 30 --font-size 15

cargo test -p noa-vt             # one crate's tests
cargo test -p noa-grid deferred  # one test by name substring
cargo run -p noa-pty --example probe   # diagnose pty/shell spawn issues

scripts/bundle-macos.sh          # → target/release/Noa.app (ad-hoc signed)
scripts/gen-icon.sh              # regenerate assets/noa.icns
```

Most tests live in each crate's `src/tests.rs` (`noa-vt`, `noa-grid`) or inline `#[cfg(test)]` modules. `noa-render/tests/pipeline.rs` is a headless GPU regression test (see GPU gotchas below).

### Sandbox constraints (important — these fail confusingly)

- Dependencies are already fetched, so **run cargo with `--offline` inside the sandbox** (`~/.cargo` writes are blocked; `failed to write cache … Operation not permitted` is a harmless warning). Only the very first dependency fetch/compile needs `dangerouslyDisableSandbox`.
- **`noa-pty` tests need real device (openpty) access → they only pass with the sandbox disabled** (sandboxed runs get `PermissionDenied`).
- GUI launch (`cargo run -p noa`) needs a display; visual parity is verified by hand.

## Architecture

A Cargo workspace mirroring Ghostty's reusable-core / platform-apprt split. The dependency DAG (enforced by convention, checkable with `cargo tree`):

```
noa-core  (primitive types: Color, CellAttrs, geometry)
   ↓
noa-vt    (from-scratch VT parser + Handler trait)        ← fidelity core
   ↓
noa-grid  (Terminal/Screen/cursor/modes; impls Handler)   ← fidelity core
noa-font  (font-kit discovery → swash raster → etagere atlas)
noa-pty   (portable-pty spawn + reader/writer threads)
   ↓
noa-render (wgpu instanced-cell renderer, surface-less)
   ↓
noa-app   (winit apprt: event loop, io thread, input encoding)
   ↓
bin/noa   (thin binary → noa_app::run())
```

**Dependency rule: only `noa-app` and `noa-render` may touch `wgpu`, and only `noa-app` may touch `winit`.** Everything at `noa-grid` and below is GUI-agnostic and unit-testable without a window or GPU. Do not leak windowing deps downward.

The **VT parser (`noa-vt`) and terminal state model (`noa-grid`) are written from scratch** — no `vte` / `alacritty_terminal`. This is deliberate: it is the fidelity core the whole clone exists to get right. Each crate names its Ghostty analog in its `lib.rs` doc comment (e.g. `noa-vt` ↔ `Parser.zig`+`stream.zig`, `noa-grid` ↔ `Terminal.zig`+`Screen.zig`).

### The two key seams

- **`noa_vt::Handler` trait** (`noa-vt/src/handler.rs`) is the parse↔state boundary. `Stream` decodes parser `Action`s and calls `Handler` methods; `noa-grid`'s `Terminal` implements them. To add VT behavior: add/extend a `Handler` method, dispatch to it in `stream.rs`, implement it on `Terminal`. Reports the terminal writes back (DA/DSR) are queued and drained via `Terminal::take_pending_writes()`.
- **`noa_render::FrameSnapshot`** (`FrameSnapshot::from_terminal(&term)`) is the state↔GPU boundary. The renderer never sees `Terminal`; it consumes an immutable per-frame snapshot, so the terminal lock is held only long enough to copy it.

### Runtime data flow (two threads)

1. **io thread** (`noa-app/src/io_thread.rs`) owns the `Pty` outright (`Pty` is `Send` but **not `Sync`**, so it can't live behind the shared `Arc`). It reads pty bytes → feeds them through one long-lived `Stream` into the `Arc<Mutex<Terminal>>` → drains `take_pending_writes()` back to the pty → pokes the event loop with `UserEvent::Redraw`. Resize requests arrive from the main thread over a `crossbeam-channel`.
2. **main thread** (`noa-app/src/app.rs`) is the winit `ApplicationHandler`: owns the window/surface/renderer, and on redraw locks the terminal only to build a `FrameSnapshot`, then renders + presents. macOS requires presenting on the window-owning thread.

Resize is **grid-first**: resize the shared `Terminal` grid *before* sending the new winsize to the pty (`on_resize` in `app.rs`), so the shell's SIGWINCH repaint isn't fed into a stale grid. (This is a grid resize, not soft-wrap reflow — reflow lands in inc≥3.)

## GPU gotchas (silent at build time, crash at runtime)

wgpu validation errors are not caught by the compiler — they abort inside the winit macOS delegate (non-unwinding). `noa-render/tests/pipeline.rs` guards against them by building the pipeline + drawing one frame on a real adapter headlessly (skips if no GPU). Two recurring traps:

- **Uniform buffer layout must match between Rust `#[repr(C)]` (4-byte align) and WGSL std140 (16-byte align).** Order fields vec4/mat4 first, then vec2 groups, then scalar padding last (trailing `vec3` forces 16-byte align — use three scalars instead). Keep `noa-render/src/instance.rs` and `src/shaders/cell.wgsl` in lockstep.
- **A bind group's `visibility` must list every shader stage that actually uses the binding.** The glyph atlas is sampled in the vertex stage (`textureDimensions`), so it needs `VERTEX_FRAGMENT`, not `FRAGMENT`.

## PTY gotcha

**`portable-pty`'s `CommandBuilder` cannot replace `argv[0]`**, so the classic login-shell `-zsh` argv0 trick is impossible (passed as an argument it becomes `zsh -zsh` → bad option → shell exits 1 → app closes on launch). Use the `-l` flag for a login shell instead (works for zsh/bash/sh; interactive mode is automatic since the pty slave is a tty).
