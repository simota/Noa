# Spec: Theme Selection (theme-selection)

> **Historical baseline:** L0/FRAME preserve the pre-implementation state as of spec authoring.
> The 574-theme catalog, settings UI, and live config reload including themes are now implemented.
> Refer to `docs/FEATURES.md` and current symbols for the present state; treat the line numbers
> below as design-time evidence.

## Metadata

- slug: `theme-selection`
- title: Theme Selection
- status: **locked** (sign-off 2026-07-02)
- owner: simota
- build-path: **orbit loop (engine: codex)** — see the "Build-path decision" section at the end for details
- recipe: /nexus spec — FRAME ✓ / EXPAND ✓ / CHALLENGE ✓ / SHAPE ✓ / SPECIFY ✓ / Quality Gate PASS (re-inspected by Judge) / LOCK ✓

## L0 — Vision

1. **Audience**: terminal users driven by dotfiles who are expected to be switching from Ghostty. noa currently supports only a single hardcoded theme, with no way to change the color scheme.
2. **Job**: write `theme = <name>` (and in the future `light:X,dark:Y`) in Ghostty-syntax config, and the color scheme used in Ghostty is reproduced as-is in noa.
3. **Success criteria**: a theme of the same name looks identical to Ghostty (bg/fg, cursor, selection colors, ANSI 256 palette), and TUIs such as vim/tmux render in the intended colors.
4. **Scope boundary**: features Ghostty lacks, such as a GUI theme editor, are out of scope (fidelity principle). This stays within the range of the existing plan (inc-4 / REQ-THEME-001).
5. **Constraints**: `noa_render::Theme` is the seam (zero renderer changes), orthogonal to OSC dynamic colors. Since runtime reload isn't implemented, v1 applies the theme at startup only (Ghostty supports reload → documented as a fidelity gap).

### Reuse / constraint findings (Lens reuse-scan)

- Theme selection is an **increment of the existing plan**: README inc-4 "~460 themes," parity-plan Phase 3-4, `REQ-THEME-001` = Partial.
- The seam to parameterize is `noa_render::Theme` (crates/noa-render/src/theme.rs:13-27) — `default_fg/bg`, `cursor`, `selection_*`, `search_*`, `palette:[Rgb;256]`. `Theme::new()` is the sole hardcoded theme.
- OSC 4/10/11/12 dynamic colors (`TerminalColors`, crates/noa-grid/src/osc.rs) are already implemented and are **already orthogonally composed** with the static Theme via `resolve_with_colors` — no conflict resolution is needed.
- `noa-config` is being replaced by a Ghostty-config-syntax parser as an increment. `theme` continues to be accepted as a v1-recognized scalar key; unknown keys change to warn + ignore.
- The CLI only has `--cols/--rows/--font-size`. The precedence model (CLI > file > default) is already established.
- **Not yet implemented**: a theme catalog (no data or generation pipeline), a macOS appearance-change hook, runtime config reload (`REQ-CONFIG-002` = Missing).
- Dependency rule: theme resolution (name → Theme) is pure data and can sit below `noa-app`/`noa-render`. No wgpu/winit contact needed.

### JTBD (Plea — all items are hypotheses)

1. `theme = <name>` makes Ghostty's color scheme work as-is (addressing the #1 reason for switch-back churn)
2. Automatic tracking of macOS light/dark switching (`light:X,dark:Y`) — **out of v1 scope**
3. Listing and easy trial via something like `noa +list-themes` — **DEFER for v1**
4. Cursor color and selection highlight change consistently too (avoiding distrust from a half-baked implementation)
5. TUIs that depend on the ANSI 16-color palette (vim/tmux/htop) behave as intended after a theme change

## L1 — Requirements

### Functional Requirements

**Config key / parsing**

- **R-1**: `noa-config`'s Ghostty-syntax parser treats the string key `theme` as a v1-recognized scalar key. The value is parsed as an optionally-quoted Ghostty-syntax value and stored in `ConfigOverrides.theme: Option<String>`. Truly unsupported keys go through the ghostty-config diagnostics path as warn + ignore.
- **R-2**: When `theme`'s value is in `light:X,dark:Y` form (a paired syntax with `light:`/`dark:` prefixes), the parser emits a dedicated diagnostic at parse time and does not accept the value. The message uses different wording from the unknown-key warn or invalid-value warn, clearly stating the cause: "the paired syntax is unsupported — specify a single name instead." Partial acceptance, such as reading only one side, is prohibited.
- **R-3**: At theme-name resolution time (in the `noa-app` layer), if the specified name doesn't exist in the bundled catalog, emit a `log::warn!` and fall back to the default theme. Startup continues (no hard fail — matching Ghostty's actual behavior).

**noa-theme crate / catalog**

- **R-4**: Add a new crate `noa-theme`. Its only dependency is `noa-core`; it must have no dependency at all on `wgpu`/`winit`/`noa-render`/`noa-app` — a pure-data crate.
- **R-5**: Vendor the full set of pre-generated theme files Ghostty distributes (the unpacked contents of the iTerm2-Color-Schemes-derived `ghostty-themes.tgz`, a snapshot of ~460 files) under `noa-theme`. Pin the upstream commit (or release tag), and attach a single attribution manifest recording the source, pinned commit, retrieval date, and license location.
- **R-6**: Add a new `scripts/gen-themes` that outputs a static Rust table (a generated artifact) inside `noa-theme` from the vendored theme files. The generated artifact is committed to the repo; `build.rs` is not used (finalized ruling: committed codegen).

**Theme resolution**

- **R-7**: A resolved theme name can construct a `noa_render::Theme` (containing `default_fg`/`default_bg`/`cursor`/`selection_fg`/`selection_bg`/`palette`). `noa-app` uses this `Theme` at startup as `GpuState.theme` (shared by all tabs, a single point of wiring). The remaining 4 fields of `Theme` (`search_fg`/`search_bg`/`active_search_fg`/`active_search_bg`) have no corresponding key in Ghostty theme files, so in v1 they keep their current hardcoded values and remain **outside theme application** (documented explicitly to prevent scope drift).
- **R-8**: When a vendored theme file lacks `selection-background`/`selection-foreground`, derive them using the same rule as Ghostty's runtime inversion fallback (swapping foreground and background). When `cursor-color` is absent, likewise follow the rule noa's existing default theme uses (cursor = default_fg).

**Grid-based color propagation**

- **R-9**: Add "theme-derived base colors" (`default_fg`/`default_bg`/`cursor`/`palette[256]`) to `noa-grid::TerminalColors`, independent of the existing dynamic OSC-override `Option<Rgb>` field group. `Terminal::new(GridSize)`'s signature does not change; base colors are injected via an additional post-construction call (a setter) (finalized ruling: grid propagation is included in v1).
- **R-10**: OSC 10/11/12 query responses report the active theme's base color — instead of a hardcoded xterm default — whenever the corresponding dynamic override hasn't been set.
- **R-11**: OSC 104 (palette reset), 110/111/112 (fg/bg/cursor reset), and RIS (`ESC c`)/`full_reset` make the active theme's base colors the post-reset baseline (clearing only the dynamic-override layer, while the base colors themselves are preserved).

**Precedence**

- **R-12**: In v1, the only source for theme selection is the config file's `theme` key. No CLI flag (`--theme`) is added; `ConfigOverrides`'s merge process carries no CLI-sourced value for theme (out of scope: `--theme` CLI flag DEFERred).

### Non-Functional Requirements (NFR)

- **NFR-1 (Fidelity)**: The resolved `Theme`'s color values (`default_fg`/`default_bg`/`cursor`/`selection_*`/`palette[]`) must match the hex values recorded in the corresponding vendored Ghostty theme file byte-for-byte (string comparison). Approximate or visual comparison is not acceptable.
- **NFR-2 (Startup cost)**: Theme-name resolution completes with a static-table lookup alone, performing no runtime file I/O or network access. Its complexity is capped at O(log n) (binary search over a sorted static array), adding no perceptible startup delay.
- **NFR-3 (Dependency hygiene)**: `noa-theme` and `noa-config`'s dependency graphs must not include `wgpu`/`winit` (verifiable via `cargo tree`).
- **NFR-4 (Backward compatibility)**: `Terminal::new(size: GridSize) -> Self`'s signature, and its existing 21 call sites (1 in production code at app.rs:332, the other 20 in test fixtures), remain unchanged.
- **NFR-5 (Quality gate)**: `cargo test --workspace` and `cargo clippy --workspace` stay clean after this change. Suppressing issues via newly added `#[allow(...)]` is prohibited.
- **NFR-6 (Attribution management)**: the vendor corpus's attribution manifest records the upstream repository name, pinned commit SHA, retrieval date, and license location; `scripts/gen-themes` exits non-zero if the manifest is missing.
- **NFR-7 (Offline generation)**: `scripts/gen-themes` performs no network access (fetching/updating vendor files is a separate process). `cargo build --workspace --offline` succeeds without re-running generation (consistent with CLAUDE.md's sandbox constraints).

## L2 — Detail

Defines only per-crate seams (no code is written here).

### noa-theme (new crate)

- Location: `crates/noa-theme/`. Added as a workspace member. `Cargo.toml`'s only dependency is `noa-core`.
- Public type: `ThemeDef` (`name: &'static str`, `default_fg`/`default_bg`/`cursor`/`selection_fg`/`selection_bg`: `Rgb`, `palette: [Rgb; 256]`). Selection/cursor inversion derivation (R-8) is **resolved to a concrete value at codegen time** — `scripts/gen-themes` runs the derivation logic in one place, so `ThemeDef` always holds only concrete values (never an `Option`) as pure data.
- Public function: `pub fn resolve(name: &str) -> Option<&'static ThemeDef>`. Implemented as `binary_search_by` over a sorted static array `&'static [(&'static str, ThemeDef)]`, adding no new crate dependency (e.g. `phf`).
- Generated artifact: `crates/noa-theme/src/generated.rs` (committed, overwritten by `scripts/gen-themes`). `src/lib.rs` holds only the `ThemeDef` type definition and the `resolve` implementation, pulling it in via `mod generated;`.
- Vendor layout: `crates/noa-theme/vendor/themes/*.conf` (native Ghostty syntax, filename = theme name) + `crates/noa-theme/vendor/ATTRIBUTION.md` (upstream repository, pinned commit SHA, retrieval date, license location).
- Unsupported keys (including 1.2.0+ special values like `cell-foreground`/`cell-background`) are ignored at codegen time (forward-compat skip, does not fail generation). Whether to support them is left as an Open Question (this L2 only establishes that "ignoring them doesn't break anything").

### noa-config

- Keep the public `theme: Option<String>` in `ConfigOverrides`/`StartupConfig`, set by the `theme` branch of the Ghostty-syntax parser (per R-12, the CLI side never carries a theme value, so `merge`'s CLI-argument side always has `theme: None`).
- Integrate `theme` parsing into the ghostty-config increment's `parser.rs`:
  - Accept `theme = 3024 Day` and `theme = "3024 Day"` as equivalent.
  - When the value starts with `light:` or `dark:`, return R-2's dedicated diagnostic and leave `ConfigOverrides.theme` as `None`.
  - **No catalog name-existence check happens here** (`noa-config` keeps its design of not depending on `noa-theme`). The existence check + fallback is `noa-app`'s responsibility.
- Assume the existing TOML-oriented `SUPPORTED_KEYS`/`reject_unknown_keys`/`toml_edit`/`parse_theme(path, document)` have already been removed as part of the ghostty-config increment.
- Tests: `theme_key_is_accepted` (accepts valid theme names both quoted and unquoted), `light_dark_syntax_is_rejected` (verifies R-2's diagnostic), and verifying unknown keys are warn+continue rather than hard errors.

### noa-grid

- Add theme-derived base-color fields to `TerminalColors` (osc.rs), separate from the existing `Option<Rgb>` dynamic-override field group (`base_fg: Rgb`, `base_bg: Rgb`, `base_cursor: Rgb`, `base_palette: [Rgb; 256]`). Its `Default` impl initializes these with the current `DEFAULT_FG`/`DEFAULT_BG`/`DEFAULT_CURSOR`/`xterm_palette()`, fully preserving existing behavior when no theme is injected (NFR-4).
- Add setter: `TerminalColors::set_base_colors(fg, bg, cursor, palette)` (additive, non-destructive). Add a thin same-named pass-through `Terminal::set_base_colors(..)` on `Terminal` too, leaving `Terminal::new(GridSize)`'s own signature untouched.
- Swap `query_default_fg`/`query_default_bg`/`query_cursor`/`query_palette`'s (osc.rs:97-112) fallback targets from the hardcoded `DEFAULT_FG`/`DEFAULT_BG`/`DEFAULT_CURSOR`/`xterm_palette_color(index)` to `self.base_fg`/`self.base_bg`/`self.base_cursor`/`self.base_palette[index]` (R-10). OSC 104/110/111/112's reset handlers themselves need no change — resetting `Option` to `None` automatically becomes theme-relative through the changed fallback target.
- `full_reset` (terminal.rs:354-364, the RIS path) currently reinitializes even the base colors via `self.colors = TerminalColors::default()`. Replace this with a reconstruction that preserves base colors while clearing only the dynamic-override layer (e.g. a "base-preserving reset" constructor/method such as `TerminalColors::with_base(fg, bg, cursor, palette)`) (R-11).

### noa-app

- Add `pub theme: Option<String>` to `AppConfig` (app.rs:37).
- `bin/noa/src/main.rs`: bridge the `theme` obtained from `noa_config::load_startup_config` into `noa_app::AppConfig` (no CLI flag added, per R-12).
- Extend `crates/noa-app/src/theme.rs`'s `default_theme()` into `resolve_theme(name: Option<&str>) -> Theme`:
  - `name` is `None` → the current `Theme::default()`.
  - `name` is `Some` and `noa_theme::resolve` hits → build a `noa_render::Theme` from the `ThemeDef`.
  - `name` is `Some` but misses → `log::warn!` (R-3) + fall back to `Theme::default()`.
- Replace `GpuState.theme`'s construction site (app.rs:283) from `crate::theme::default_theme()` with `crate::theme::resolve_theme(config.theme.as_deref())`.
- At each tab/window's `Terminal` creation site, immediately after creation call `terminal.set_base_colors(theme.default_fg, theme.default_bg, theme.cursor, theme.palette)` to seed the grid's base colors (invoking the noa-grid-side setter).
- Add `noa-theme` as a new dependency in `crates/noa-app/Cargo.toml` (since `noa-app` sits at the top of the DAG and `noa-theme` itself gains no GUI dependency, this doesn't conflict with the existing dependency rules).

### scripts/gen-themes

- A Bash script of the same shape as `scripts/gen-icon.sh` (`set -euo pipefail`, safe to re-run, no destructive operations).
- Input: `crates/noa-theme/vendor/themes/*.conf` + `crates/noa-theme/vendor/ATTRIBUTION.md`.
- Processing: parse each theme file in native Ghostty syntax (recognized keys: `background`/`foreground`/`cursor-color`/`cursor-text`/`selection-background`/`selection-foreground`/`palette = N=#rrggbb`; unknown keys are ignored), apply R-8's inversion derivation, and serialize as a static array sorted by name.
- Output: `crates/noa-theme/src/generated.rs` (committed).
- No network access (fetching/updating vendor files is a separate, manual, one-time vendor-update procedure, not part of this script's runtime requirements, NFR-7).
- Exits non-zero when `ATTRIBUTION.md` is missing (NFR-6).

## L3 — Acceptance Criteria

Each AC records its corresponding `R-*`/`NFR-*` (in `AC-n → R-m` form). Ripple's 3 mandatory tests are satisfied by AC-1+AC-2 (①), AC-14 (②), and AC-17 (③). Independent verification during the post-implementation Attest phase is recommended.

### Config parsing

- **AC-1 → R-1**: Given a config containing only `theme = 3024 Day` or `theme = "3024 Day"`. When `noa_config::parse_overrides` runs. Then diagnostics are empty and it returns `ConfigOverrides.theme == Some("3024 Day".to_string())`.
- **AC-2 → R-1 (Ripple mandatory test ①)**: Given a config containing a truly unsupported key (e.g. `bogus-key = x`) followed by a valid key. When `parse_overrides` runs. Then it does not error, one warn diagnostic containing the file path and `bogus-key` is added, and parsing of the subsequent key continues.
- **AC-3 → R-2**: Given a config containing `theme = light:Foo,dark:Bar`. When `parse_overrides` runs. Then it does not error, a dedicated diagnostic is generated stating that the `light:`/`dark:` paired syntax is unsupported, and `ConfigOverrides.theme == None` (confirming this diagnostic's wording differs from the generic unknown-key/invalid-value diagnostic).
- **AC-4 → R-3**: Given a theme name not present in the catalog. When `resolve_theme(Some("NoSuchTheme"))` is called (a unit test, no GPU/window context needed). Then it doesn't error and returns a `Theme` equivalent to `Theme::default()`. Additionally confirm via code inspection (grep) that a `log::warn!` call exists on the fallback path (no log-capture harness is introduced).

### noa-theme crate / catalog

- **AC-5 → R-4**: Given `crates/noa-theme` is a workspace member. When `cargo tree -p noa-theme --offline` runs. Then the dependency graph contains no noa crate other than `noa-core`, and no `wgpu`/`winit`.
- **AC-6 → R-5, NFR-6**: Given `crates/noa-theme/vendor/ATTRIBUTION.md` exists. When its contents are inspected. Then the 4 items — upstream repository name, pinned commit SHA, retrieval date, license location — are all recorded.
- **AC-7 → R-6, NFR-7**: Given `crates/noa-theme/src/generated.rs` is committed to the repo and no `build.rs` exists. When `cargo build --workspace --offline` runs. Then the build succeeds, `generated.rs` is not regenerated, and no network access occurs.

### Theme resolution / fidelity

- **AC-8 → R-7**: Given `theme = "<known-name>"` is configured. When `resolve_theme` constructs a `noa_render::Theme`. Then all 6 fields — `default_fg`/`default_bg`/`cursor`/`selection_fg`/`selection_bg`/`palette` — exactly match the corresponding `ThemeDef`'s field values (the comparison must not produce a false negative even for fields that happen to coincide with the default value).
- **AC-9 → R-7, NFR-1 (spot check, mandatory)**: Given 3 or more known themes arbitrarily selected from the vendored set (substituting the actual vendored canonical names). When each theme name is passed to `resolve_theme` and the resulting `default_fg`/`default_bg`/`cursor`/`palette[]` are cross-checked against the hex values in the corresponding vendored Ghostty theme file. Then every field matches byte-for-byte (string comparison).
- **AC-10 → R-8**: Given a vendored theme file without `selection-background`/`selection-foreground`. When `scripts/gen-themes` generates the `ThemeDef`. Then `selection_bg == default_fg` and `selection_fg == default_bg` (inversion fallback).
- **AC-11 → R-8**: Given a vendored theme file without `cursor-color`. When `scripts/gen-themes` generates the `ThemeDef`. Then `cursor == default_fg`.

### Grid base-color propagation

- **AC-12 → R-9 (regression guard)**: Given `TerminalColors::default()` (base colors not injected). When `query_default_fg`/`query_default_bg`/`query_cursor`/`query_palette` are called. Then they return the same values as before the change (`DEFAULT_FG`/`DEFAULT_BG`/`DEFAULT_CURSOR`/`xterm_palette_color`).
- **AC-13 → R-9**: Given a `Terminal` on which `Terminal::set_base_colors(fg, bg, cursor, palette)` has been called. When its internal `TerminalColors` is inspected with no dynamic OSC override applied. Then the base-color fields match the injected values.
- **AC-14 → R-10 (Ripple mandatory test ②)**: Given the active theme's `default_bg` differs from noa's hardcoded default, with no OSC 11 dynamic override applied. When an OSC 11 query (`\x1b]11;?\x1b\\`) is processed as a byte stream from the pty. Then the response returned by `take_pending_writes()` reports the active theme's `default_bg` (not the hardcoded xterm default).
- **AC-15 → R-10**: Given the same conditions for OSC 10 (default_fg) and OSC 12 (cursor), with no dynamic override. When each query is processed. Then the response reports the active theme's `default_fg`/`cursor`.
- **AC-16 → R-11**: Given `default_bg` is temporarily overridden via OSC 11, followed by an OSC 111 (reset). When a subsequent OSC 11 query is sent. Then the response is not the hardcoded xterm default, but the active theme's `default_bg`.
- **AC-17 → R-11 (Ripple mandatory test ③)**: Given part of the palette overridden via OSC 4 and `default_bg` overridden via OSC 11. When RIS (`ESC c`)/`full_reset` is executed. Then all subsequent OSC 4/10/11/12 queries report the active theme's base colors (palette included), never reverting to the hardcoded xterm default.

### Precedence / dependencies / quality

- **AC-18 → R-12**: Given `bin/noa/src/main.rs`'s `Args` (clap definition). When running `noa --help` or inspecting the source. Then no `--theme` flag exists, and the only input source for theme is the config file's `theme` key.
- **AC-19 → NFR-2**: Given `noa_theme::resolve`'s implementation. When the implementation is inspected. Then code review confirms zero runtime file I/O or network access (only a `binary_search_by` over a static array). As a reference figure, a microbenchmark sanity-checks that a single lookup's cost is negligible relative to the overall startup sequence (target <1ms) — informational, not a pass/fail criterion.
- **AC-20 → NFR-3**: Given `crates/noa-theme` and `crates/noa-config`'s `Cargo.toml`. When `cargo tree -p noa-theme --offline` and `cargo tree -p noa-config --offline` run. Then neither dependency graph shows `wgpu`/`winit`.
- **AC-21 → NFR-4**: Given the changed `noa-grid::Terminal`. When `fn new(size: GridSize) -> Self`'s signature and its existing 21 call sites (git diff) are checked. Then there is no diff in either the signature or the call sites (base-color injection is implemented as an additional call after `Terminal::new`).
- **AC-22 → NFR-5 (mandatory)**: Given a workspace with this change applied. When `cargo test --workspace --offline` and `cargo clippy --workspace --offline` run. Then both complete with exit code 0, and no newly added `#[allow(...)]` exists as a result of this change.
- **AC-23 → NFR-6**: Given `crates/noa-theme/vendor/ATTRIBUTION.md` does not exist. When `scripts/gen-themes` runs. Then it fails with a non-zero exit code and a message indicating the missing manifest.
- **AC-24 → NFR-7**: Given `scripts/gen-themes`. When (a) the script is run inside a network-blocked sandbox, and (b) the script body is grepped for `curl`/`wget`/`git fetch`/`git clone`/`nc`/`scp`/`ssh`. Then (a) completes successfully and (b) finds no instruction that performs network access (the sandboxed run is authoritative, not the deny-list grep alone).

### Traceability summary

| Requirement | AC | Requirement | AC | Requirement | AC |
|---|---|---|---|---|---|
| R-1 | AC-1, AC-2 | R-8 | AC-10, AC-11 | NFR-2 | AC-19 |
| R-2 | AC-3 | R-9 | AC-12, AC-13 | NFR-3 | AC-20 |
| R-3 | AC-4 | R-10 | AC-14, AC-15 | NFR-4 | AC-21 |
| R-4 | AC-5 | R-11 | AC-16, AC-17 | NFR-5 | AC-22 |
| R-5 | AC-6 | R-12 | AC-18 | NFR-6 | AC-6, AC-23 |
| R-6 | AC-7 | NFR-1 | AC-9 | NFR-7 | AC-7, AC-24 |
| R-7 | AC-8, AC-9 | | | | |

All 19 requirements (R-1〜R-12, NFR-1〜NFR-7) have ≥1 corresponding AC (traceability completeness 19/19 = 100%).

## Scope

### Problem

noa currently supports only a single hardcoded theme, with no way to change the color scheme. Users switching from Ghostty who are driven by dotfiles expect that writing `theme = <name>` in the config reproduces their existing Ghostty color scheme (bg/fg, cursor, selection colors, ANSI 256 palette) exactly, and that TUIs like vim/tmux render in the intended colors. Failing to meet this expectation becomes a primary reason for switch-back churn.

### Proposed solution

Vendor the theme files Ghostty generates and distributes (the iTerm2-Color-Schemes-derived `ghostty-themes.tgz`), and pin them into the repo as committed codegen (`scripts/gen-themes` → a static table inside the new crate `noa-theme`). Add a `theme = <name>` key to `noa-config`, resolving the name at startup → building a `noa_render::Theme` (zero renderer seam changes). Further propagate the resolved theme's base colors (bg/fg, etc.) into `noa-grid`'s `TerminalColors`, making OSC 10/11/12 query responses and OSC 104/110-112 resets theme-relative. Unknown theme names emit a warn and fall back to the default (no hard fail). When a theme file lacks selection/cursor colors, fill them in via the same inversion derivation Ghostty uses (resolved to a concrete value at codegen time).

### In-scope

- `noa-config`: continue accepting `theme = <name>` as a v1-recognized scalar key in Ghostty syntax
- New crate `noa-theme` (depends only on `noa-core`, pure data)
- `scripts/gen-themes`: generates a static Rust table inside `noa-theme` from vendored theme files, and commits the generated artifact (no build.rs)
- Vendor target: the full set of Ghostty-distributed theme files (~460 snapshot files) + a pinned upstream commit + a single attribution manifest
- App wiring: resolve `theme` at startup and construct a `noa_render::Theme` (via `GpuState.theme`, shared by all tabs)
- Warn + default fallback on unknown theme names
- Inversion-based color fallback derivation for themes missing selection/cursor
- Grid base-color seeding: add base-color fields to `TerminalColors`, extending `Terminal::new` non-destructively (no impact on the existing 21 call sites)
- Ripple's 3 mandatory tests: ① `theme` key accepted / unknown key rejected, ② OSC 11 query reports the active theme's bg, ③ RIS/`full_reset` restores theme-relative state
- Aligning `noa-config`'s unknown-key validation with ghostty-config's warn+continue semantics

### Out-of-scope

- `light:X,dark:Y` syntax and automatic macOS-appearance switching — "accepting the syntax with no actual switching" is prohibited (silent fidelity divergence). **Explicitly out of scope**: reject that syntax with a clear error when passed (partial acceptance is not acceptable). Documented as a fidelity gap.
- Runtime config reload (theme changes apply only at startup)
- A CLI subcommand equivalent to `+list-themes`
- The `--theme` CLI flag
- Lookup of user files under the config dir's `themes/` (v1 keeps a structure that could support this in a future increment, but does not implement it)
- GUI theme editor / color picker

### Assumptions

- Vendored theme files are parsed only against the same syntax as Ghostty config (no full config-grammar parser is implemented)
- That OSC 104 reset restores to theme-relative state is PARTIAL verification (an estimate) — adopted as an implementation premise (see Open Questions)
- "~460 themes" is a snapshot count as of ingestion and may shift with upstream updates (the pinned commit freezes the count)
- Selection/cursor inversion derivation follows Ghostty's documented behavior ("If this is not set, then the selection color is inverted"), resolved to a concrete value at codegen time

## Considered but rejected

User selection (EXPAND checkpoint, 2026-07-02): **Direction B (full catalog ingestion) adopted** (the ingestion mechanism was ruled on as committed codegen during CHALLENGE).

- **A. Minimal static slice (manually port 20-30 themes)** — rejected: falls short of README inc-4's "~460 themes" commitment. Manual porting becomes throwaway cost as the catalog grows.
- **C. Dual lookup (`themes/` user files)** — rejected (v1): a real Ghostty mechanism, but v1 satisfies JTBD① with the bundled catalog. The structure remains extensible for a later increment.
- **D. Light/dark auto-switching as the centerpiece** — rejected (v1): would make the macOS appearance hook + runtime re-theming a v1-mandatory dependency. "Accepting the syntax alone" is prohibited → `light:X,dark:Y` is documented as explicitly out of scope.
- **build.rs codegen (the contested ruling on the ingestion mechanism, Magi confidence 85)** — rejected: no precedent for build.rs in the repo, a network footgun in the offline sandbox; adopted committed codegen of the same shape as the existing `scripts/gen-icon.sh` instead (Ripple's proposal).
- **DEFERring grid propagation (a contested ruling, Void confidence 75)** — rejected: vim/tmux's automatic background detection via OSC 11 queries (JTBD⑤) would misbehave, so it's included in v1. The fix is cheap and non-destructive.

## Final decisions (approved by the user at LOCK)

1. Ingestion mechanism = **committed codegen** (`scripts/gen-themes`'s output is committed, no build.rs)
2. Theme propagation to the grid = **included in v1** (R-9〜R-11)
3. `+list-themes` = **DEFER** (candidate for a future increment)
4. `--theme` CLI flag = **DEFER** (candidate for a future increment)

## Open Questions / Deferred Decisions

- The exact attribution requirements (license wording, placement) — to be finalized by checking iTerm2-Color-Schemes' LICENSE at vendor-ingestion time (NFR-6's 4 items are the minimum requirement)
- The exact count of bundled themes — to be finalized at ingestion time and recorded in ATTRIBUTION.md
- Confirm the semantics of OSC 104/110-112's theme-relative restoration against Ghostty source (currently PARTIAL — the override/default two-layer structure referenced in discussions/12708 is circumstantial evidence). If a discrepancy surfaces during implementation, revise R-11 to match Ghostty's actual behavior
- Whether v1 needs to support `cell-foreground`/`cell-background` (Ghostty 1.2.0+ special values) — only confirmed that codegen ignores them (doesn't break)
- The config file path was changed as part of the ghostty-config increment to `<config_dir>/noa/config` (no extension). The old `config.toml` is detected with a warn only and not loaded.
- Future increments (deferred): `light:X,dark:Y` + macOS appearance hook / runtime reload / `themes/` user-file lookup / `+list-themes` / `--theme`

## Build-path decision

**orbit loop (engine: codex)** — selected at sign-off (2026-07-02).

- This spec's AC-1〜AC-24 serve as the loop's completion contract (a machine-checkable DONE gate). AC-22 (`cargo test`/`clippy` green) is each iteration's verification command; AC-9 (byte-match spot check) is the fidelity gate.
- Execution engine: **Codex CLI** (each iteration executed by codex). Prerequisite: `~/.codex/config.toml` has `multi_agent = true` + `[agents] max_depth >= 2`. Same operational pattern as the existing `.nexus/loops/*` (`exec-codex.sh`).
- Vendor ingestion (a one-time process involving network access) is separated out as a manual/pre-step outside the loop (NFR-7).
- Handoff target: the `orbit` agent (`~/.claude/skills/orbit/SKILL.md`, engine details in `orbit/reference/executor-engines.md`). **This spec does not write code — generating/launching the loop is executed via separate instruction.**
