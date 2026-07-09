# Repository Guidelines

## Project Structure & Module Organization

`noa` is a Rust 2024 Cargo workspace for a macOS-first terminal emulator.
Workspace members are `crates/*` plus the thin binary in `bin/noa`.

- `crates/noa-core`: shared primitives such as colors, attributes, and geometry.
- `crates/noa-vt`: from-scratch ANSI/DEC VT parser and stream dispatch.
- `crates/noa-grid`: terminal state, cursor, modes, rows, and scrolling.
- `crates/noa-font`: system font discovery, rasterization, and glyph atlas data.
- `crates/noa-theme`: vendored Ghostty-compatible theme catalog (574 themes).
- `crates/noa-config`: config discovery, parsing, validation, and precedence.
- `crates/noa-render`: `wgpu` renderer, shaders, snapshots, and theme mapping.
- `crates/noa-pty`: PTY spawning plus reader/writer threads.
- `crates/noa-app`: `winit` app loop, input, IO thread, and UI integration.
- `assets/` and `scripts/`: app icon assets and macOS packaging helpers.

Keep dependency boundaries intact: only `noa-app` and `noa-render` should use
`wgpu`, and only `noa-app` should use `winit`.

## Build, Test, and Development Commands

- `cargo build --workspace`: build every crate and binary.
- `cargo test --workspace`: run unit and integration tests, including VT/grid
  conformance tests and renderer smoke tests.
- `cargo run -p noa -- --cols 100 --rows 30 --font-size 15`: launch the
  terminal with explicit startup dimensions.
- `scripts/bundle-macos.sh`: build the release binary and assemble
  `target/release/Noa.app`.
- `cargo fmt --all`: format Rust code before submitting changes.

## Coding Style & Naming Conventions

Use idiomatic Rust with `rustfmt` defaults and 4-space indentation. Name crates
with the `noa-*` pattern, modules and functions in `snake_case`, and
types/traits in `UpperCamelCase`. Keep lower-level crates GUI-agnostic. Comments
should explain terminal semantics, platform constraints, or parity decisions.

## Testing Guidelines

Use Rust's built-in test harness. Place unit tests near the crate module or in
files such as `crates/noa-vt/src/tests.rs`; use `crates/<name>/tests/` for
integration checks. Prefer deterministic byte-sequence to
action/grid assertions for VT and grid behavior. Renderer tests may skip
gracefully when no GPU adapter is available. Run `cargo test --workspace` before
opening a pull request.

## Commit & Pull Request Guidelines

Recent history follows Conventional Commits, for example `feat(macos): ...` and
`fix(render): ...`. Keep commit subjects imperative, scoped, and under about
72 characters. Pull requests should explain the behavior change, list the test
commands run, link relevant issues, and include screenshots or recordings for
visible terminal or macOS app changes.

## Security & Configuration Tips

Do not vendor or copy Ghostty source code; this project is an independent
reimplementation verified by observable parity. Avoid committing local terminal
captures, secrets, shell histories, or generated `target/` artifacts.
