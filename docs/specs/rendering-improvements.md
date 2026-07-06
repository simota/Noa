# Rendering Improvements

## Metadata

- slug: rendering-improvements
- feature title: Rendering-Quality Improvements (fallback, color emoji, shaping/ligatures, native AA, dirty-row diffing)
- status: locked
- owner: noa maintainers
- current phase: LOCKED
- scope mode: Full (16 functional + non-functional requirements, 5 work
  packages, 18 acceptance criteria)
- build-path decision: implementation loop consumes the L3 acceptance
  criteria directly as the machine-checkable completion contract; work
  packages land in the WP0 → WP4 sequence (D5).
- decision provenance: L0/L1/L2 grounded in the Phase 1 Lens
  current-state map (exact file/line evidence) and **bound by the Phase 3
  Magi verdict** (decisions D1–D5, AC seed, OUT list, failure
  conditions). The Magi verdict is BINDING; this spec refines its AC seed
  into traceable AC-IDs and adds WP4.
- scope override (orchestrator, binding): the Magi verdict defers WP4
  (dirty-row diffing + atlas eviction). The user-approved scope **pulls
  dirty-row diffing back IN** as the final work package (WP4). **Atlas
  eviction stays deferred/OUT** per Magi.
- key bound decisions: D1 shaper = **rustybuzz** (pure-Rust HarfBuzz
  port); D2 shaping integration = **run shaper in noa-font,
  segmentation render-side, ligature = one glyph instance anchored at
  cluster-start cell**; D3 AA = **native gamma-correct only** (skip
  subpixel — already integer; skip real thicken); D4 emoji = **two
  atlases (R8 mask + RGBA8 color) + FLAG_COLOR_GLYPH**, with the
  single-RGBA8 fallback recorded as the D4-Sophia escape hatch; D5
  sequencing = **WP0 → WP1 → WP2 → WP3 → WP4**.

## L0 — Vision

### Problem

`noa`'s renderer is correct for plain monospaced ASCII but diverges from
Ghostty's observable output on macOS the moment text needs real font
handling. Concretely (Phase 1 Lens):

- **Color emoji render as monochrome silhouettes.** The glyph atlas is
  R8 alpha-only (`noa-render/src/renderer.rs:83`); `noa-font`'s rasterizer
  discards the RGBA color bitmap down to its alpha channel
  (`noa-font/src/raster.rs:58-66`), and the shader tints the result with
  the cell's foreground color (`noa-render/src/shaders/cell.wgsl:113-116`).
  No emoji fallback family is ever requested
  (`noa-font/src/face.rs:72-135`).
- **No text shaping.** The glyph cache key is a single `char`
  (`noa-font/src/lib.rs:21-25`); resolution is codepoint-by-codepoint
  with no script-run coherence (`noa-font/src/grid.rs:95-105`). There is
  no shaping engine, so ligatures never form and combining marks are
  positioned independently at the same cell origin with their own pen
  bearing (`noa-render/src/renderer.rs:589-602`) rather than attached as a
  shaped cluster.
- **No font configuration.** Only `font-size` is parsed; `font-family` is
  recognized but rejected with a "not yet supported" diagnostic
  (`noa-config/src/parser.rs:55`). There is no family/weight/style,
  feature, or variation-axis configuration surface.
- **No gamma-correct antialiasing.** Only solid theme/cell colors are
  sRGB-converted (`noa-render/src/renderer.rs:841-861`); glyph coverage is
  blended in the wrong space (`cell.wgsl:113-116`), so thin dark-on-light
  text is visibly thinned relative to Ghostty's `native` (macOS default)
  blending.
- **Full-frame rebuild every redraw.** `FrameSnapshot::from_terminal`
  clones all visible rows and `rebuild_panes` clears and rebuilds the
  entire instance list every frame (`noa-render/src/snapshot.rs:24-40`,
  `renderer.rs:161-193`), with no per-row dirty signal — a real per-frame
  cost that multiplies with split-pane counts.

### Audience

- **macOS users of `noa`** expecting Ghostty's text rendering: colored
  emoji, programming-font ligatures, correct-weight antialiased text.
- **Contributors**: this is the fidelity core the clone exists to get
  right — the VT/grid layers are faithful, but the glyph pipeline is the
  remaining observable gap on macOS-first.

### Job To Be Done

Make `noa`'s rendered text and emoji indistinguishable from Ghostty's on
macOS for the in-scope cases — colored emoji, configurable fonts, shaped
ligatures and combining clusters, and `native` gamma-correct antialiasing
— without regressing VT/grid conformance, the headless GPU pipeline test,
or frame time; and add per-row dirty diffing so unchanged rows cost
nothing to re-render.

### Success Definition

For the in-scope cases, rendered output matches Ghostty's `native`-mode
macOS output on hand-verified visual checks and passes the machine-checkable
acceptance criteria (`cargo test` unit, headless GPU pipeline, config
parse). The pure-Rust / headless-testable invariant (D1/D2) and the
dependency rule (wgpu only in `noa-app`/`noa-render`, winit only in
`noa-app`) hold. `cargo test --workspace` and `cargo clippy --workspace`
stay green. Shaping does not regress frame time (WP2 shape cache), and
after WP4 an unchanged row triggers zero instance rebuild.

### Reversibility / Learning

- **Reversibility: MEDIUM overall, incrementally landable.** Each WP
  lands independently on the WP0 → WP4 seam; reverting a single WP is a
  single-PR revert. **Exception (LOW): D2 shaping API shape** — the
  `ShapedGlyph` contract is a sticky seam; getting it wrong forces a wide
  re-touch of `renderer.rs`. Mitigation: freeze the `shape_run` /
  `ShapedGlyph` signature (L2 below) before building WP2 consumers.
- **Learning / fail thresholds** are the Failure Conditions section — any
  triggered condition invalidates the current WP's plan and reroutes per
  its escalation.

## Scope

### In scope

- **WP0** — config signature + plumbing: real parse path for
  `font-family` and per-style variants, `font-feature`, `font-variation*`,
  `font-synthetic-style`, plus accepted-but-deferred `font-thicken*` /
  `alpha-blending`; `FontGrid::new(px)` → `FontGrid::new(px, font_cfg)`.
- **WP1** — fallback + color emoji: Apple Color Emoji probe; RGBA8 second
  atlas; `FLAG_COLOR_GLYPH` passthrough; two-atlas bind-group layout.
- **WP2** — shaping + ligatures + shape cache: `rustybuzz` `shape_run`;
  render-side run segmentation from `FrameSnapshot`; ligature →
  cluster-start-cell mapping; combining-mark attachment; per-run shape
  cache; `font-feature`-gated ligatures (OFF by default).
- **WP3** — native gamma-correct AA: `native`-mode coverage blend in
  lockstep with `target_format_is_srgb`.
- **WP4** — dirty-row diffing: per-row dirty signal in `noa-grid`
  Screen/Row; per-row instance patching in `noa-render` `rebuild_panes`.

### Out of scope

Copied from Magi verdict §3 (dirty-row diffing removed — now WP4; atlas
eviction stays OUT):

- **CoreText / harfbuzz-rs FFI shaping** — `rustybuzz` only (D1).
  CoreText-shaper divergence accepted as a documented deviation.
- **`linear` / `linear-corrected` alpha-blending** and **Display-P3 color
  space** — deferred; correction-curve formula uncertain and non-default
  on macOS (D3a). Keys accepted-but-fall-back-with-diagnostic.
- **Subpixel horizontal positioning** — permanently out; integer snap is
  the parity behavior (D3b). No work — already integer end-to-end.
- **`font-thicken` real implementation** — deferred; CoreText-only, off by
  default; key accepted-noop-with-diagnostic (D3c).
- **Atlas eviction** — deferred; current no-evict + warn is acceptable
  until emoji/variations inflate the atlas past the 8192² cap; the D4
  two-atlas design keeps the R8 atlas lean, so this deferral stays cheap.
- **`minimum-contrast` nudge**, **`adjust-*` metric options**,
  **`freetype-load-flags`** — not in this chain; formula/algorithm
  undocumented. Config keys may be parsed-but-noop if trivial, else out.
- **Soft-wrap reflow** — pre-existing inc≥3 boundary (CLAUDE.md).
- **Cursor-over-ligature exact trigger** — use the safe default (break run
  at cursor cell); byte-exact match deferred.
- Already excluded by the chain: **IME preedit rework**, **window
  opacity/blur**.

## Config surface

Every config key introduced or touched by this feature. "Kind" is either
**real** (parsed and consumed) or **parsed-fallback-diagnostic** (parsed,
validated, but its effect is a documented fall-back with a precise
diagnostic — never a silent no-op or a crash). All keys land their parse
path at `noa-config/src/parser.rs:55` (replacing the current
list-key-diagnostic branch for `font-family`) with `StartupConfig` /
`ConfigOverrides` fields added in `noa-config/src/lib.rs`.

| Key | Syntax (example) | Repeatable | Default | Kind | WP |
|---|---|---|---|---|---|
| `font-family` | `font-family = JetBrains Mono` | yes (stack) | macOS `Menlo` first, then system monospace/fallbacks | real | WP0 |
| `font-family-bold` | `font-family-bold = JetBrains Mono Bold` | yes | derived from `font-family` | real | WP0 |
| `font-family-italic` | `font-family-italic = JetBrains Mono Italic` | yes | derived from `font-family` | real | WP0 |
| `font-family-bold-italic` | `font-family-bold-italic = …` | yes | derived from `font-family` | real | WP0 |
| `font-feature` | `font-feature = calt` / `font-feature = -liga` | yes | none (liga/calt/dlig OFF) | real (consumed WP2) | WP0/WP2 |
| `font-variation` | `font-variation = wght=700` | yes | none | real (consumed WP2) | WP0/WP2 |
| `font-variation-bold` | `font-variation-bold = wght=800` | yes | none | real (consumed WP2) | WP0/WP2 |
| `font-variation-italic` | `font-variation-italic = slnt=-10` | yes | none | real (consumed WP2) | WP0/WP2 |
| `font-variation-bold-italic` | `font-variation-bold-italic = …` | yes | none | real (consumed WP2) | WP0/WP2 |
| `font-synthetic-style` | `font-synthetic-style = true` / `no-bold` | no | `true` (synthesize bold+italic) | real (consumed WP2, nice-to-have) | WP0/WP2 |
| `alpha-blending` | `alpha-blending = native` | no | `native` | real for `native`; **parsed-fallback-diagnostic** for `linear` / `linear-corrected` (fall back to `native`) | WP0/WP3 |
| `font-thicken` | `font-thicken = true` | no | `false` | **parsed-fallback-diagnostic** (accepted-noop; deferred impl) | WP0 |
| `font-thicken-strength` | `font-thicken-strength = 128` | no | `255` | **parsed-fallback-diagnostic** (accepted-noop; deferred impl) | WP0 |

## L1 — Requirements

Priority: **[Must]** blocks the WP; **[Should]** = nice-to-have within
scope (may ship in a follow-up patch to its WP without failing the WP).

### Functional — WP0 (config signature + plumbing)

- **REQ-CFG-1** [Must]: `font-family` and its per-style variants
  (`font-family-bold` / `-italic` / `-bold-italic`) parse and validate
  through the real parse path — **not** the "not yet supported"
  list-key-diagnostic. Invalid values (empty name) produce a precise
  diagnostic.
- **REQ-CFG-2** [Must]: `FontGrid::new(px)` becomes
  `FontGrid::new(px, font_cfg)`; all **14** call sites are updated in
  the same WP so later WPs fill the config struct without re-breaking
  the signature: 4 in `noa-app` (`app.rs:370,401,1069,1414`) and 10
  test sites (`noa-font/src/grid.rs:139,169,193`,
  `noa-render/tests/pipeline.rs:103,132,220,279,347,445`,
  `noa-render/src/renderer.rs:917`), which pass
  `FontConfig::default()`.
- **REQ-CFG-3** [Must]: `font-feature`, `font-variation` (+ per-style
  variants), and `font-synthetic-style` parse and validate into config
  fields (consumed in WP2). Malformed values (e.g. a feature tag that is
  not 4 chars, a variation axis without `=value`) produce a precise
  diagnostic.
- **REQ-CFG-4** [Must]: Non-`native` `alpha-blending` values and any
  `font-thicken` / `font-thicken-strength` value parse and **fall back
  with a diagnostic** — no silent no-op, no crash. `alpha-blending =
  native` is a real value consumed by WP3.

### Functional — WP1 (fallback + color emoji)

- **REQ-EMOJI-1** [Must]: `load_font_stack` (`noa-font/src/face.rs`)
  probes and includes **Apple Color Emoji** in the resolved fallback
  stack; an emoji codepoint resolves to it rather than to a tofu/blank.
- **REQ-EMOJI-2** [Must]: Color emoji rasterize as **RGBA** (color bitmap
  preserved, not reduced to R8 alpha) into a second **RGBA8 atlas**; a
  glyph flagged `FLAG_COLOR_GLYPH` samples the RGBA8 atlas as a passthrough
  (no foreground-color tint). Text glyphs continue to use the R8 mask
  atlas.
- **REQ-EMOJI-3** [Must]: The two-atlas bind-group layout builds and draws
  one frame without a wgpu validation error; an atlas realloc rebuilds
  **both** atlases' bind groups. `instance.rs` `#[repr(C)]` and
  `cell.wgsl` std140 stay in lockstep when adding `FLAG_COLOR_GLYPH`; the
  bind-group `visibility` lists every stage that samples each atlas
  (CLAUDE.md GPU gotcha).

### Functional — WP2 (shaping + ligatures + shape cache)

- **REQ-SHAPE-1** [Must]: A new `noa-font` `shape_run` API shapes a run of
  cells via `rustybuzz`, returning `ShapedGlyph` records whose HarfBuzz
  **cluster** value maps each glyph to its source cell. A ligature is one
  shaped glyph spanning N cells; it is emitted as **one CellInstance
  anchored at the cluster-start cell**, and the cells it covers emit their
  background + decoration but **suppress their own glyph** (no
  double-draw).
- **REQ-SHAPE-2** [Must]: Ligature features (`liga`/`calt`/`dlig`) are
  **OFF by default**; `!=`, `->` etc. render as separate glyphs.
  Configuring `font-feature = calt` (etc.) enables the ligated glyph.
- **REQ-SHAPE-3** [Must]: `rustybuzz` and `swash` receive **identical
  variation-axis coordinates** for a given `font-variation` config, so a
  shaped glyph's advance matches its rasterized glyph (no glyph/advance
  drift).
- **REQ-SHAPE-4** [Must]: A combining mark + base in one cell shapes as an
  **attached cluster** — the accent is positioned by the shaped
  `x_offset`/`y_offset`, not by an independent per-char pen bearing.
- **REQ-SHAPE-5** [Should]: A **per-run shape cache**, keyed by
  `(run text, style, font_cfg hash)`, avoids re-invoking `rustybuzz` for an
  unchanged run on the next frame. (Load-bearing perf mitigation — D2
  dissent: shaping must not regress frame time before WP4 lands.)
- **REQ-SHAPE-6** [Should]: Run **segmentation is render-side** (fed from
  `FrameSnapshot`), breaking runs at row / font-face / style / selection /
  cursor boundaries; a Latin+CJK mixed row segments into ≥2 runs at the
  face boundary and shapes each with its own face. `noa-grid` stays
  GUI-agnostic — only a per-cell style-key accessor may be exposed, a
  render-seam concern, not a Handler-seam change.
- **REQ-SHAPE-7** [Should]: `font-synthetic-style` synthesizes bold/italic
  when the family lacks the native style; a per-style disable
  (e.g. `no-bold`) is respected.

### Functional — WP3 (native gamma-correct AA)

- **REQ-AA-1** [Must]: `native` (macOS default) gamma-correct coverage
  blending is active — a non-sRGB surface with a straight-alpha coverage
  blend in gamma space, kept in **lockstep with `target_format_is_srgb`**
  (`renderer.rs:841-861`) so there is no double-gamma and no no-gamma
  artifact; thin dark-on-light text is not visibly thinned.
- **REQ-AA-2** [Must]: Glyph positions remain **integer-snapped** (no
  subpixel phase); the existing bearing-conversion tests stay green.
  Integer snap *is* the parity behavior (D3b) — no work beyond preserving
  it.

### Functional — WP4 (dirty-row diffing)

- **REQ-PERF-1** [Must]: `noa-grid` `Screen`/`Row` gain a **per-row dirty
  signal**; a row is marked dirty when its contents change and clean once
  its instances are rebuilt. The signal is GUI-agnostic (no wgpu/winit in
  `noa-grid`).
- **REQ-PERF-2** [Must]: `noa-render` `rebuild_panes` **patches only dirty
  rows'** instance ranges instead of clear-and-rebuild-all. An unchanged
  frame produces **zero instance rebuild for unchanged rows**.
- **REQ-PERF-3** [Must]: Dirty-row diffing is **output-identical** to the
  full rebuild — the rendered frame for any given terminal state is the
  same whether produced by full rebuild or per-row patch.

### Non-Functional (cross-WP)

- **REQ-NF-1** [Must]: `cargo build --workspace`, `cargo test --workspace`,
  and `cargo clippy --workspace` stay green after every WP.
- **REQ-NF-2** [Must]: The build stays **pure-Rust** and
  **headless-testable** — no C toolchain (rules out harfbuzz-rs FFI /
  CoreText shaping); every machine-checkable AC runs under `cargo test`
  or the headless GPU pipeline test.
- **REQ-NF-3** [Must]: The dependency rule holds — `wgpu` only in
  `noa-app`/`noa-render`, `winit` only in `noa-app`; shaping additions
  live in `noa-font` + the render seam, never leaking windowing deps into
  `noa-grid`/`noa-vt`.
- **REQ-NF-4** [Must]: The headless GPU pipeline test
  (`noa-render/tests/pipeline.rs`) stays green across WP1 (two-atlas),
  WP3 (blend), and WP4 (per-row patch) — no wgpu validation abort in the
  winit macOS delegate.

## L2 — Detail

### Sequencing (D5)

WP0 → WP1 → WP2 → WP3 → WP4. Ordering serializes churn on the two hot
shared files — **`renderer.rs`** (WP1/WP2/WP3/WP4) and **`pipeline.rs`**
(WP1) — and lands the constructor-signature change (`FontGrid::new`) once,
up front, so later WPs fill the config struct without re-breaking call
sites. WP4 lands last because per-row patching must diff against the
final instance-build path (after shaping and two-atlas changes settle).

### WP0 — Config signature + plumbing (`noa-config`, `noa-app`)

- **Goal**: real config parse path + one-time `FontGrid::new` signature
  change, with all downstream fields present (even if minimally consumed
  until later WPs).
- **Change sites**:
  - `noa-config/src/parser.rs:55` — replace the
    `"keybind" | "palette" | "font-family"` list-key-diagnostic branch:
    route `font-family*`, `font-feature`, `font-variation*`,
    `font-synthetic-style` to real parse handlers; route `alpha-blending`,
    `font-thicken`, `font-thicken-strength` to
    parsed-fallback-diagnostic handlers.
  - `noa-config/src/lib.rs` — add fields to `StartupConfig`
    (and `ConfigOverrides` + `merge`/`apply_to`) for a `FontConfig`
    sub-struct: family stack + per-style families, feature list,
    variation-axis list (+ per-style), synthetic-style mode, plus the
    accepted-but-deferred `alpha_blending` / `thicken` fields.
  - `noa-font` — `FontGrid::new(px)` → `FontGrid::new(px, font_cfg)`;
    `load_font_stack` gains the config input (consumed for real in WP1/WP2).
  - `noa-app` — update the 4 call sites (`app.rs:370,401,1069,1414`).
  - Test call sites — update the 10 test constructors to pass
    `FontConfig::default()`: `noa-font/src/grid.rs:139,169,193`,
    `noa-render/tests/pipeline.rs:103,132,220,279,347,445`,
    `noa-render/src/renderer.rs:917`.
- **Dependencies**: none (lands first).

### WP1 — Fallback + color emoji (`noa-font`, `noa-render`)

- **Goal**: colored emoji via an Apple Color Emoji probe + an RGBA8 second
  atlas + a `FLAG_COLOR_GLYPH` passthrough. Self-contained visual win with
  no shaping dependency, landed before WP2 so glyph resolution is stable.
- **Change sites**:
  - `noa-font/src/face.rs` (`load_font_stack`) — add an "Apple Color
    Emoji" probe to the fallback stack.
  - `noa-font/src/raster.rs:58-66` — keep the RGBA color-bitmap output
    (do not reduce to `px[3]` alpha); add an RGBA raster path.
  - `noa-font/src/atlas.rs` + `lib.rs:26-37` (`GlyphInfo`) — a second
    RGBA8 `Atlas` (or a channel-generic atlas) + a format flag on
    `GlyphInfo`.
  - `noa-render/src/instance.rs` — `FLAG_COLOR_GLYPH` bit (keep
    `#[repr(C)]` ↔ std140 lockstep).
  - `noa-render/src/shaders/cell.wgsl` (`fs_main`, ~`:113-116`) — on
    `FLAG_COLOR_GLYPH`, sample the RGBA8 atlas as passthrough instead of
    `vec4(color.rgb, color.a * coverage)`.
  - `noa-render/src/renderer.rs:83,301-329` + `pipeline.rs:36-38` — second
    atlas texture bound; bind-group-layout change; realloc rebuilds both
    atlases' bind groups. **Watch the CLAUDE.md visibility gotcha**: only
    add `VERTEX_FRAGMENT` visibility for the color atlas if a vertex-stage
    `textureDimensions` on it is actually introduced (the mask atlas
    already needs it; the color atlas is fragment-sampled).
- **Fallback (D4-Sophia, recorded)**: if the two-atlas bind-group work
  destabilizes the headless GPU test, fall back to **single-RGBA8 for all
  glyphs** as an interim — behaviorally correct (emoji in color), just
  memory-heavy — and defer the two-atlas split. This is the escape hatch,
  not the plan.
- **Dependencies**: WP0 (config-carrying `FontGrid::new`).

### WP2 — Shaping + ligatures + shape cache (`noa-font`, `noa-render`)

- **Goal**: real shaping via `rustybuzz`, ligatures gated on
  `font-feature`, combining-mark attachment, and a per-run shape cache so
  shaping does not regress frame time.
- **Frozen seam (LOW-reversibility — freeze before building consumers)**:

  ```
  // noa-font
  struct ShapeCell { ch: char, /* + combining chars */, style_key: StyleKey }
  struct ShapedGlyph {
      glyph_id: u16,
      face_id:  FaceId,
      x_advance: i32,
      x_offset:  i32,
      y_offset:  i32,
      cluster:   u32,   // source cell index (HarfBuzz semantics)
  }
  fn shape_run(cells: &[ShapeCell], font_cfg: &FontConfig) -> Vec<ShapedGlyph>;
  ```

- **Change sites**:
  - `noa-font/Cargo.toml` — add `rustybuzz` (pure-Rust HarfBuzz port; D1).
  - `noa-font` — new `shape_run` module + a **run-based cache key**
    replacing/extending the single-`char` `GlyphKey` (`lib.rs:21-25`);
    pass the **same** variation-axis coords to both `rustybuzz` and
    `swash` (D1 constraint — shaper and rasterizer must see the same
    `wght`); `liga`/`calt`/`dlig` default-OFF then apply `font-feature`.
  - `noa-font` — per-run **shape cache** keyed by
    `(run text, style, font_cfg hash)` (D2 mitigation, REQ-SHAPE-5).
  - `noa-render/src/renderer.rs:589-602` — the per-cell glyph loop
    consumes **row-level shaped runs**: build runs from `FrameSnapshot`,
    break at row/face/style/selection/cursor boundaries, emit ligature
    instances at the cluster-start `grid_pos`, suppress covered cells'
    glyphs. `grid_pos` stays the anchor; the "1-glyph-per-cell" model
    relaxes to "≤1 glyph per cell, positioned by shaped offset".
  - `noa-render`/`noa-app` — segmentation is render-side; if
    `FrameSnapshot` lacks per-cell style context, **extend the snapshot**
    (preferred) rather than adding a grid-layer accessor (failure
    condition 4).
- **Dependencies**: WP0 (feature/variation config), WP1 (final
  glyph-resolution + atlas paths).

### WP3 — Native gamma-correct AA (`noa-render`)

- **Goal**: Ghostty's `native` (macOS default) gamma-correct coverage
  blend; non-`native` `alpha-blending` values fall back with a diagnostic.
- **Change sites**:
  - `noa-render/src/shaders/cell.wgsl` (`fs_main`, ~`:113-116`) — coverage
    blend in gamma space for `native`.
  - `noa-render/src/renderer.rs:57,119,841-861` — keep the coverage path
    in **lockstep with `target_format_is_srgb`** to avoid the double-gamma
    trap (CLAUDE.md GPU gotcha); non-`native` `alpha-blending` falls back
    to `native` with the WP0 diagnostic.
- **Dependencies**: WP2 (the coverage/instance path is final after
  shaping).

### WP4 — Dirty-row diffing (`noa-grid`, `noa-render`)

- **Goal**: per-row dirty tracking so unchanged rows cost nothing to
  re-render, output-identical to the full rebuild.
- **Change sites**:
  - `noa-grid` `Screen`/`Row` — add a per-row dirty flag: set on any cell
    mutation, cleared when consumed. GUI-agnostic; no wgpu/winit.
  - `noa-render/src/snapshot.rs:24-40` — carry the per-row dirty signal
    into `FrameSnapshot` (clone/diff only changed rows).
  - `noa-render/src/renderer.rs:161-193,468-611` — `rebuild_panes` /
    `rebuild_cell_instances` **patch per-row instance ranges** instead of
    clear+rebuild across the 3 concatenated passes (bg / glyph /
    decoration, `renderer.rs:606-608`); unchanged rows keep their prior
    instances and buffer regions.
- **Dependencies**: WP1/WP2/WP3 (patch against the final instance-build
  path). Lands last.

## L3 — Acceptance Criteria

Method tags: **[unit]** `cargo test -p <crate>`; **[headless]**
`noa-render/tests/pipeline.rs` GPU test (skips without an adapter);
**[visual]** hand-verified per CLAUDE.md. Priority: **[MH]** must-have,
**[NH]** nice-to-have.

### WP0

- **AC-WP0-01** [MH] [unit] (REQ-CFG-1) — Given a config with `font-family
  = JetBrains Mono` and each per-style variant, When parsed, Then no "not
  yet supported" diagnostic is emitted and the families land in
  `FontConfig`; an empty family value yields a precise diagnostic.
  `cargo test -p noa-config`.
- **AC-WP0-02** [MH] [unit] (REQ-CFG-2) — Given the `FontGrid::new(px,
  font_cfg)` signature with all 14 call sites updated (4 `noa-app` + 10
  test sites), When the workspace builds and test targets compile, Then
  `cargo build --workspace`, `cargo clippy --workspace`, **and
  `cargo test --workspace --offline`** are clean (`cargo build` alone
  does not compile test targets, so the test-site breakage would
  otherwise slip through).
- **AC-WP0-03** [MH] [unit] (REQ-CFG-3) — Given `font-feature = calt`,
  `font-variation = wght=700`, and `font-synthetic-style = no-bold`, When
  parsed, Then each lands in `FontConfig`; a malformed feature tag or a
  variation axis missing `=value` yields a precise diagnostic.
  `cargo test -p noa-config`.
- **AC-WP0-04** [MH] [unit] (REQ-CFG-4) — Given `alpha-blending = linear`,
  `font-thicken = true`, and `font-thicken-strength = 128`, When parsed,
  Then each parses and produces a fall-back diagnostic (no silent no-op,
  no crash); `alpha-blending = native` produces no diagnostic.
  `cargo test -p noa-config`.

### WP1

- **AC-WP1-01** [MH] [unit] (REQ-EMOJI-1) — Given the resolved fallback
  stack from `load_font_stack`, When an emoji codepoint is resolved, Then
  it resolves to Apple Color Emoji, not a tofu/blank. `cargo test -p
  noa-font`.
- **AC-WP1-02** [MH] [headless]+[visual] (REQ-EMOJI-2) — Given an emoji
  glyph, When rasterized, Then it is RGBA (not R8 alpha) and a
  `FLAG_COLOR_GLYPH` glyph samples the RGBA8 atlas as passthrough with no
  foreground tint (headless color-passthrough assertion + visual check).
- **AC-WP1-03** [MH] [headless] (REQ-EMOJI-3) — Given the two-atlas
  bind-group layout, When one frame draws, Then it completes with no wgpu
  validation error; When an atlas realloc occurs, Then both atlases' bind
  groups are rebuilt (extends `pipeline.rs:342`).

### WP2

- **AC-WP2-01** [MH] [unit] (REQ-SHAPE-1) — Given `shape_run` over a run
  containing a ligature, When shaped, Then the HarfBuzz cluster values map
  the ligature glyph to its cluster-start cell and the covered cells emit
  no duplicate glyph. `cargo test -p noa-font`.
- **AC-WP2-02** [MH] [unit]+[visual] (REQ-SHAPE-2) — Given a run with `!=`
  / `->`, When ligature features are default, Then they render as separate
  glyphs; When `font-feature = calt` is configured, Then the ligated glyph
  is produced. `cargo test -p noa-font` + visual.
- **AC-WP2-03** [MH] [unit] (REQ-SHAPE-3) — Given `font-variation =
  wght=700`, When the same run is shaped by `rustybuzz` and rasterized by
  `swash`, Then both receive identical variation coords and the advances
  match (no drift). `cargo test -p noa-font`.
- **AC-WP2-04** [MH] [unit]+[visual] (REQ-SHAPE-4) — Given a combining mark
  + base in one cell, When shaped, Then the accent is positioned by the
  shaped `x_offset`/`y_offset` as an attached cluster, not by an
  independent per-char pen bearing. `cargo test` + visual.
- **AC-WP2-05** [NH] [unit] (REQ-SHAPE-5) — Given an unchanged run on two
  consecutive frames, When shaped, Then `rustybuzz` is invoked once and the
  second frame is a cache hit (cache-hit counter or equivalent).
  `cargo test -p noa-font`.
- **AC-WP2-06** [NH] [unit] (REQ-SHAPE-6) — Given a Latin+CJK mixed row,
  When segmented, Then it splits into ≥2 runs at the face boundary and each
  run shapes with its own face. `cargo test -p noa-font`.
- **AC-WP2-07** [NH] [unit]+[visual] (REQ-SHAPE-7) — Given a family lacking
  a native bold, When `font-synthetic-style = true`, Then bold is
  synthesized; When `no-bold`, Then bold synthesis is disabled.
  `cargo test -p noa-font` + visual.

### WP3

- **AC-WP3-01** [MH] [visual]+[unit] (REQ-AA-1) — Given `native`
  alpha-blending on macOS, When thin dark-on-light text renders, Then the
  coverage blend is gamma-correct with no double-gamma against
  `target_format_is_srgb` (text is not visibly thinned). Visual +
  `renderer.rs` unit test on the gamma conversion.
- **AC-WP3-02** [MH] [unit] (REQ-AA-2) — Given the glyph positioning path,
  When instances are built, Then positions remain integer-snapped (no
  subpixel phase) and the existing bearing-conversion tests stay green.
  `cargo test -p noa-render`.

### WP4

- **AC-WP4-01** [MH] [unit] (REQ-PERF-1) — Given a `noa-grid` `Screen`,
  When a single cell in one row mutates, Then only that row's dirty flag is
  set and all other rows stay clean; When instances are rebuilt, Then the
  flag clears. `cargo test -p noa-grid`.
- **AC-WP4-02** [MH] [unit] (REQ-PERF-2) — Given a frame in which no row
  changed since the last rebuild, When `rebuild_panes` runs, Then zero
  instances are rebuilt for the unchanged rows (rebuild counter is 0).
  `cargo test -p noa-render`.
- **AC-WP4-03** [MH] [unit]+[headless] (REQ-PERF-3) — Given identical
  terminal state, When rendered once via full rebuild and once via per-row
  patch, Then the resulting instance lists (and the headless-drawn frame)
  are identical. `cargo test -p noa-render` + headless.

### Cross-WP (non-functional)

- **AC-NF-01** [MH] [unit]+[headless] (REQ-NF-1, REQ-NF-4) — Given the full
  new test set across all WPs, When `cargo test --workspace` and
  `cargo clippy --workspace` run, Then all pass with no `#[ignore]` beyond
  the documented pty sandbox constraint and the headless pipeline test
  stays green.
- **AC-NF-02** [MH] [unit] (REQ-NF-2, REQ-NF-3) — Given `cargo tree`, When
  inspected, Then no C-toolchain shaping dep (harfbuzz-rs) is present,
  `noa-grid`/`noa-vt` depend on neither `wgpu` nor `winit`, and every
  machine-checkable AC runs headlessly. `cargo test` + `cargo tree`.

## Traceability

Bidirectional REQ ↔ AC. Every REQ has ≥1 AC; every AC traces to ≥1 REQ.

| REQ | AC(s) | Priority |
|---|---|---|
| REQ-CFG-1 | AC-WP0-01 | Must |
| REQ-CFG-2 | AC-WP0-02 | Must |
| REQ-CFG-3 | AC-WP0-03 | Must |
| REQ-CFG-4 | AC-WP0-04 | Must |
| REQ-EMOJI-1 | AC-WP1-01 | Must |
| REQ-EMOJI-2 | AC-WP1-02 | Must |
| REQ-EMOJI-3 | AC-WP1-03 | Must |
| REQ-SHAPE-1 | AC-WP2-01 | Must |
| REQ-SHAPE-2 | AC-WP2-02 | Must |
| REQ-SHAPE-3 | AC-WP2-03 | Must |
| REQ-SHAPE-4 | AC-WP2-04 | Must |
| REQ-SHAPE-5 | AC-WP2-05 | Should |
| REQ-SHAPE-6 | AC-WP2-06 | Should |
| REQ-SHAPE-7 | AC-WP2-07 | Should |
| REQ-AA-1 | AC-WP3-01 | Must |
| REQ-AA-2 | AC-WP3-02 | Must |
| REQ-PERF-1 | AC-WP4-01 | Must |
| REQ-PERF-2 | AC-WP4-02 | Must |
| REQ-PERF-3 | AC-WP4-03 | Must |
| REQ-NF-1 | AC-NF-01 | Must |
| REQ-NF-2 | AC-NF-02 | Must |
| REQ-NF-3 | AC-NF-02 | Must |
| REQ-NF-4 | AC-NF-01, AC-WP1-03, AC-WP4-03 | Must |

**Coverage: 23/23 requirements traced to ≥1 AC = 100%.** (Full-scope
minimum ≥95%.) 21 ACs total (18 must-have / 3 nice-to-have); every AC
names its source REQ.

## Failure Conditions / Rollback

From Magi verdict §4 — any triggered condition invalidates the current
plan mid-implementation and reroutes per its escalation.

1. **`rustybuzz` cannot share variation/face state with `swash`** such
   that advances drift (AC-WP2-03 fails, unreconcilable) → escalate D1:
   pin variations OFF for v1, or reconsider `swash::shape` / CoreText FFI.
2. **The shape cache does not recover per-frame cost** and shaping every
   visible row regresses frame time → **pull WP4 (dirty-row diffing)
   forward** ahead of WP3, changing the sequence. (WP4 is already in
   scope, so this is a reorder, not new work.)
3. **Two-atlas bind-group change destabilizes the headless GPU test**
   (recurring wgpu validation aborts in the winit delegate) → invoke the
   D4-Sophia dissent: fall back to **single-RGBA8** interim, defer the
   two-atlas split.
4. **`FrameSnapshot` cannot carry enough per-cell style/attr context** for
   render-side run segmentation without a grid-layer change that breaks
   the GUI-agnostic invariant → re-open D2: **extend the snapshot**
   (preferred) or accept a narrowly-scoped grid accessor with explicit
   justification.
5. **`rustybuzz` output diverges visibly from Ghostty/CoreText** on a
   common macOS script (e.g. a shipped programming font's ligatures) → the
   D1 "documented deviation" assumption breaks; escalate to a human on the
   CoreText-shaper question.
6. **`native` gamma parity requires a non-sRGB surface reconfiguration**
   that conflicts with the existing solid-color sRGB path
   (`renderer.rs:841-861`) and cannot be made lockstep → D3a native-only
   may ship behind a flag or defer, reducing WP3 to integer-snap-only.

**Rollback**: each WP is a single-PR revert on the WP0 → WP4 seam
(MEDIUM reversibility). The one LOW-reversibility surface is the WP2
`shape_run` / `ShapedGlyph` contract — freeze it before building
`renderer.rs` consumers; reverting it after consumers exist is a wide
re-touch, not a single revert.

## Risk Register

| Risk | Source | Sev | Mitigation (AC) | Monitor |
|---|---|---|---|---|
| Per-frame reshape regresses frame time | D2 shaping every row | H | Per-run shape cache (AC-WP2-05); WP4 pull-forward (fail cond. 2) | Frame time / cache-hit rate |
| Two-atlas bind-group wgpu validation abort | D4 + CLAUDE.md GPU gotcha | M | Headless gate (AC-WP1-03); single-RGBA8 fallback | Headless test stability |
| Shaper ↔ raster advance drift | D1 dual consumers of variation coords | M | Identical-coords test (AC-WP2-03); pin variations off if needed | Advance-match test |
| Double-gamma / no-gamma glyph artifact | D3a native vs sRGB solid-color path | M | Lockstep with `target_format_is_srgb` (AC-WP3-01) | Visual + gamma unit test |
| `rustybuzz` ≠ CoreText on complex scripts | D1 documented deviation | L-M | Document; escalate on concrete report | User reports / CJK visual |
| `FrameSnapshot` insufficient for run segmentation | D2 seam | L-M | Extend snapshot, not grid (fail cond. 4) | WP2 integration |
| Dirty-row patch diverges from full rebuild | WP4 per-row patch | M | Output-identical test (AC-WP4-03) | Instance-list equality |
| Atlas cap hit (no eviction) once emoji/variations inflate | D5 deferral (OUT) | L | Warn already present; two-atlas keeps R8 lean | 8192² cap warnings |

## Meta

- version: 1.0 (LOCKED)
- reviews: L0/L1/L2 grounded in Phase 1 Lens; L1/L3 bound by Phase 3 Magi
  verdict (D1–D5, AC seed, OUT list, failure conditions). WP4 added per
  the binding orchestrator scope override.
- open questions:
  - **`native` surface reconfiguration** (fail cond. 6): whether
    `native` gamma parity needs a non-sRGB surface change that conflicts
    with the existing sRGB solid-color path — resolve during WP3 spike.
  - **CoreText-shaper divergence** (fail cond. 5): accepted as a
    documented deviation for v1; escalate only on a concrete
    common-script mismatch report.
- downstream: the L3 acceptance criteria are the machine-checkable
  completion contract for the implementation loop; the [visual] ACs
  (AC-WP1-02, AC-WP2-02/04/07, AC-WP3-01) are the human acceptance pass
  after the loop's [unit]/[headless] gate is green.
