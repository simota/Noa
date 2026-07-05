# Background Image — Feature Spec (LOCKED)

**Metadata**
- slug: `background-image`
- title: Terminal background image support
- status: `locked`
- owner: simota
- build-path: **apex** (autonomous end-to-end build; consumes L3 ACs as the verification contract)

---

## L0 — Vision

- **Problem**: noa renders backgrounds as a single clear color + window opacity + macOS blur only. Ghostty's `background-image*` family is unimplemented (zero code trace).
- **Audience**: noa users who customize terminal appearance; Ghostty-config importers expecting `background-image*` keys to carry over.
- **Job-to-be-done**: Lay an arbitrary image behind the terminal grid, with configurable opacity / position / fit / repeat, while preserving text legibility and coexisting with existing opacity + blur.
- **Success**: All Ghostty `background-image*` keys parse and apply; the image renders below text and above the (possibly transparent) clear color; Ghostty config import carries the keys; no regression to opacity/blur.

### Reuse & constraints (Lens)
- **Reuse — render**: `noa-render/src/image_layer.rs` `ImageLayer` + `ImageBand::BelowBackground` (z-band below the background quad) is directly extensible; `shaders/image.wgsl` straight-alpha quad reusable.
- **Reuse — config wiring**: `parser.rs` match (73-236) + `is_supported_scalar_key` (289-322) → `StartupConfig`/`ConfigOverrides` (`lib.rs`) → `bin/noa/src/main.rs` `app_config_from_startup` → `noa-app/src/app/config.rs` `AppConfig`.
- **Reuse — decode**: `png`/`flate2` already workspace deps (Kitty graphics).
- **Constraint**: transparent-surface path is duplicated across `app.rs` / `quick_terminal.rs` / `sidebar.rs` — background image must be wired into every surface-creation site.
- **Constraint**: macOS-first.

---

## Scope

### In scope
- Config-key parity with Ghostty: `background-image`, `background-image-opacity`, `background-image-position`, `background-image-fit`, `background-image-repeat`.
- PNG decode.
- Render below text, above clear color (`ImageBand::BelowBackground`), on **all** surfaces: main window, quick terminal, sidebar.
- Ghostty-faithful opacity composition (image above the color layer; only the color layer is scaled by `background-opacity`).
- Recompute placement on window resize.
- Startup-time load (config parse → decode once).

### Out of scope
- Non-PNG decode (JPEG/WebP/GIF/animated) — deferred.
- Hot-reload of the image on live config change — deferred (startup only).
- Per-pane / per-split distinct images — Ghostty scopes to the surface; not supported.
- Glass-style image transparency (image riding window transparency) — rejected in CHALLENGE.
- Non-macOS platform verification.

---

## L1 — Requirements

Functional:
- **FR-1** — Parse `background-image = <path>` (string path; `~` and env expansion follow the existing path-handling convention).
- **FR-2** — Parse `background-image-opacity = <float>` clamped `[0.0, 1.0]`, default `1.0`.
- **FR-3** — Parse `background-image-position` enum: `top-left|top-center|top-right|center-left|center|center-right|bottom-left|bottom-center|bottom-right`, default `center`.
- **FR-4** — Parse `background-image-fit` enum: `none|contain|cover|stretch`, default `contain`.
- **FR-5** — Parse `background-image-repeat` bool, default `false`.
- **FR-6** — All five keys registered in `is_supported_scalar_key` so Ghostty import carries them (not commented out).
- **FR-7** — Thread the five values through `StartupConfig`/`ConfigOverrides` (merge/apply_to), `bin/noa` `app_config_from_startup`, and `AppConfig`.
- **FR-8** — Decode the PNG at startup. Non-PNG extension/content, missing file, or decode failure → surfaced diagnostic (log/warn, not silent); background image disabled; terminal launches normally.
- **FR-9** — Render the decoded image as a quad in `ImageBand::BelowBackground` spanning the surface, below the background quad and above the clear color.
- **FR-10** — `background-image-fit` computes source↔dest mapping: `stretch` fills the surface ignoring aspect; `cover` fills preserving aspect, cropping overflow; `contain` fits inside preserving aspect (letterbox); `none` uses native pixel size.
- **FR-11** — `background-image-position` anchors the image within the surface for `contain`/`none` (the 9-anchor grid); ignored for `stretch`; sets the crop anchor for `cover`.
- **FR-12** — `background-image-repeat` tiles the image across the surface (meaningful for `none`; when the image does not cover the surface).
- **FR-13** — `background-image-opacity` scales the image quad's alpha, independent of `background-opacity`.
- **FR-14** — Placement (dest rect / tiling) recomputes on window resize.
- **FR-15** — Wired into all three surface-creation sites: main window, quick terminal, sidebar.

Non-functional:
- **NFR-1** — Startup load only; no live reload (v1).
- **NFR-2** — No crash on GPU validation, missing GPU, or bad image; degrade to no-image.
- **NFR-3** — Opacity composition matches Ghostty: color layer scaled by `background-opacity`, image drawn above at `background-image-opacity`; image's own alpha hides the desktop through a transparent window (image is not scaled by `background-opacity`).

---

## L3 — Acceptance Criteria

- **AC-1** (FR-1) — A config with `background-image = <valid.png>` parses without error and the path reaches `AppConfig`. _(unit: parser + apply_to)_
- **AC-2** (FR-2) — `background-image-opacity = 0.5` parses to `0.5`; `2.0` clamps to `1.0`; `-1` clamps to `0.0`; absent → `1.0`. _(unit: parser)_
- **AC-3** (FR-3) — Each of the 9 position tokens parses to its variant; an invalid token is a parse error; absent → `center`. _(unit: parser)_
- **AC-4** (FR-4) — Each of `none|contain|cover|stretch` parses to its variant; invalid → parse error; absent → `contain`. _(unit: parser)_
- **AC-5** (FR-5) — `background-image-repeat = true|false` parses; absent → `false`. _(unit: parser)_
- **AC-6** (FR-6) — `is_supported_scalar_key` returns true for all five keys; a Ghostty import containing them emits them uncommented. _(unit: import)_
- **AC-7** (FR-7) — An integration path from a config file with all five keys yields an `AppConfig` carrying the five resolved values. _(unit/integration)_
- **AC-8** (FR-8) — A missing path and a non-PNG file each: (a) do not panic, (b) produce a diagnostic, (c) leave the terminal running with no background image. _(unit + manual)_
- **AC-9** (FR-9, FR-13, NFR-3) — With a valid PNG, a headless render (extend `noa-render/tests/pipeline.rs`) draws one frame with the image quad present in the BelowBackground band and no wgpu validation error; the image quad's alpha reflects `background-image-opacity` while the clear color's alpha reflects `background-opacity`. _(headless GPU test)_
  - _Implementation note_: rather than reusing the per-pane kitty `ImageBand::BelowBackground` placement, the implementation draws a dedicated `BackgroundImageLayer` as a single full-surface quad at the head of the render pass — above the `LoadOp::Clear` color and below every pane's background/cell quads. This is functionally equivalent for AC-9 (below text, above clear color, spans the surface) and keeps the terminal background image independent of Kitty graphics. A tiling `repeat` uses an `AddressMode::Repeat` sampler with a UV-scaled single quad (one draw, O(1)) instead of emitting one quad per tile.
- **AC-10** (FR-10) — For a known image + surface size, the computed dest rect for each of `none|contain|cover|stretch` matches the expected rectangle (aspect-preserving math). _(unit)_
- **AC-11** (FR-11) — For `contain` with each of the 9 positions, the letterboxed image's dest-rect anchor matches expectation; `stretch` ignores position. _(unit)_
- **AC-12** (FR-12) — With `repeat = true` and an image smaller than the surface, tiling covers the full surface (tile count matches ceil(surface/image)). _(unit)_
- **AC-13** (FR-14) — After a simulated resize, the recomputed dest rect matches the new surface size. _(unit)_
- **AC-14** (FR-15) — Main window, quick terminal, and sidebar surfaces each receive the background-image config and render it (manual GUI verification; wiring covered by a code-path assertion where testable). _(manual + code review)_
- **AC-15** (NFR-2) — Running with no GPU (headless skip path) or a corrupt PNG does not abort the process. _(test skip-guard + AC-8)_

---

## Considered but rejected

- **Full `image` crate (multi-format)** — rejected for v1 to keep deps light; PNG-only. (Deferred, see Open Questions.)
- **Hot reload via config watch** — rejected for v1; startup load only. (Deferred.)
- **Glass-style image transparency** (image scaled by `background-opacity`, desktop showing through the image) — rejected in favor of Ghostty-faithful composition (image above the color layer).
- **Per-pane / per-split images** — rejected; Ghostty scopes the image to the surface/window.

---

## Open Questions / Deferred Decisions

- **OQ-1** — Verify exact Ghostty default values against the latest Ghostty docs before implementation: `fit` default (`contain` assumed), `position` default (`center`), `repeat` default (`false`), `opacity` default (`1.0`). Spec assumes these; correct at implementation if docs differ.
- **OQ-2 (deferred)** — JPEG/WebP/GIF decode via the `image` crate.
- **OQ-3 (deferred)** — Live hot-reload of the background image on config change (ride existing config watch if/when present).
- **OQ-4 (deferred)** — Animated backgrounds / GIF frames.
- **OQ-5** — `repeat` interaction with `contain`/`cover`/`stretch` (which already cover the surface): spec treats `repeat` as meaningful only when the image does not fill the surface (primarily `none`). Confirm against Ghostty behavior at implementation.

---

## Spec Quality Gate (self-review)

| Dimension | Result |
|-----------|--------|
| Ambiguity | PASS — enum value-sets, defaults, and clamp ranges are explicit. Residual: exact Ghostty defaults parked as OQ-1. |
| Completeness | PASS — every FR/NFR has ≥1 AC (FR-1→AC-1, FR-2→AC-2, FR-3→AC-3, FR-4→AC-4, FR-5→AC-5, FR-6→AC-6, FR-7→AC-7, FR-8→AC-8/15, FR-9→AC-9, FR-10→AC-10, FR-11→AC-11, FR-12→AC-12, FR-13→AC-9, FR-14→AC-13, FR-15→AC-14, NFR-2→AC-15, NFR-3→AC-9). |
| Consistency | PASS — scope (PNG-only, image-above-clear, all surfaces) matches FRs and ACs; no contradiction. |
| Testability | PASS — AC-1..7,10..13 unit; AC-9,15 headless GPU; AC-8,14 manual+unit. All machine- or human-verifiable. |
| Scope coherence | PASS — in/out-of-scope mutually exclusive (PNG vs other formats, startup vs hot-reload, surface-scope vs per-pane) and jointly cover the feature. |
