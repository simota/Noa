# Repository Guidelines

## Project Structure & Module Organization

`noa` is a Rust 2024 Cargo workspace for a macOS-first terminal emulator.
Workspace members are `crates/*`, the thin binary in `bin/noa`, and the
fixture-based parity regression harness in `tests/parity`.

- `crates/noa-core`: shared primitives such as colors, attributes, and geometry.
- `crates/noa-vt`: from-scratch ANSI/DEC VT parser and stream dispatch.
- `crates/noa-grid`: terminal state, cursor, modes, rows, and scrolling.
- `crates/noa-font`: system font discovery, rasterization, and glyph atlas data.
- `crates/noa-theme`: vendored Ghostty-compatible theme catalog (574 Ghostty
  themes plus `Noa`).
- `crates/noa-config`: config discovery, parsing, validation, and precedence.
- `crates/noa-render`: `wgpu` renderer, shaders, snapshots, and theme mapping.
- `crates/noa-pty`: PTY spawning plus reader/writer threads.
- `crates/noa-app`: `winit` app loop, input, IO thread, and UI integration.
- `assets/`: app icon, AppleScript dictionary, entitlements, and images.
- `scripts/`: macOS bundle, Linux package, and release scripts, theme and icon
  generation, PGO builds, and smoke-test helpers.

Keep dependency boundaries intact: only `noa-app` and `noa-render` may use
`wgpu`, and only `noa-app` may use `winit` (checked by `scripts/verify.sh`).
To find a feature's code and tests, see `docs/feature-map.md`.

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

Format with `rustfmt` defaults (4-space indentation). Name crates
with the `noa-*` pattern, modules and functions in `snake_case`, and
types/traits in `UpperCamelCase`. Keep lower-level crates GUI-agnostic. Comments
should explain terminal semantics, platform constraints, or parity decisions.

## Testing Guidelines

Use Rust's built-in test harness. Place unit tests near the crate module or in
files such as `crates/noa-vt/src/tests.rs`; use `crates/<name>/tests/` for
integration checks. Prefer deterministic byte-sequence to
action/grid assertions for VT and grid behavior. Renderer tests may skip
when no GPU adapter is available.

## Verification

- After every change: `scripts/verify.sh`. It runs the format and dependency
  boundary checks, plus the tests of the packages the change reaches and their
  dependents. A change to the workspace manifests, `Cargo.lock`, `vendor/`,
  `.github/` or the script itself runs everything.
- Before a pull request: `scripts/verify.sh --full`. CI runs exactly this on
  macOS and Linux; clippy (`-D warnings`) and desktop-file validation run only
  on Linux, as before.
- A failing check is named in the output; its full log is
  `.verify/<check>.log`, and timings are in `.verify/timings`.
- The script pins the CI toolchain (`RUSTUP_TOOLCHAIN=1.92.0`). In a sandbox,
  set `CARGO_NET_OFFLINE=true`; `noa-pty` tests need real `openpty` and
  `noa-ipc` tests bind a loopback port, so both fail with `PermissionDenied`
  inside it.
- GUI behaviour and visual parity have no automated check: verify them by hand
  and say so in the pull request.

## Safety

- New dependencies, `Cargo.lock` changes, `vendor/`, release signing
  (`.github/workflows/release.yml`, `scripts/release.sh`) and the gate itself
  (`scripts/verify.sh`, `.github/workflows/ci.yml`, this file) need the
  maintainer's agreement before the change is made.
- No secret, signing identity or notarization credential is printed, committed
  or pasted into a file.
- No test is deleted, skipped or `#[ignore]`d to make a check pass, and no lint
  is silenced with `#[allow]` to the same end.

## Discipline — nothing checks these

- Every rule under Safety: there is no CODEOWNERS file and `main` has no branch
  protection, so only review holds them.
- Comments explain terminal semantics, platform constraints or parity
  decisions; Ghostty source is never copied (below).

## Commit & Pull Request Guidelines

Recent history follows Conventional Commits, for example `feat(macos): ...` and
`fix(render): ...`. Keep commit subjects imperative, scoped, and under about
72 characters. Pull requests fill in `.github/pull_request_template.md`: the
behavior change, the verification run and its result, what was not verified,
linked issues, and screenshots or recordings for visible terminal or macOS app
changes.

## Security & Configuration Tips

Do not vendor or copy Ghostty source code; this project is an independent
reimplementation verified by observable parity. Avoid committing local terminal
captures, secrets, shell histories, or generated `target/` artifacts.
