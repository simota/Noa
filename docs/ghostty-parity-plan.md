# Ghostty Parity Implementation Plan

> **⚠️ Archived (historical plan document as of inc-1/inc-2).**
> This document is a plan snapshot based on the 2026-07-02 inventory and has not been kept in sync since.
> The gap descriptions from Phase 2 onward (decoration rendering, protocols, tabs/splits/search/themes/background
> opacity, etc., listed as "not implemented") are **mostly implemented by now, and the content is stale**.
> For the latest implementation status, see the Status / Roadmap sections in `README.md`.

Written 2026-07-02. A phased plan to reach "Ghostty-level" parity, based on a feature inventory taken
after inc-1 was complete and inc-2 was nearly complete (a full review of every crate, ~182 tests). It
turns the README's Roadmap (inc 2–6) into concrete phases based on measured gaps.

## Current state (summary)

**Working solidly**: nearly all CSI editing sequences / SGR 16-color, 256-color, truecolor / alt screen /
DECSTBM / DECSC・DECRC / bracketed paste / SGR mouse reporting / wide characters / resize & reflow (cursor
anchor preserved) / paged scrollback (byte-size cap via `scrollback-limit`) + selection + search engine /
OSC 0・2・4・10-12・52 / DA・DSR responses / clipboard / IME preedit / native menu / in-code keybinding
engine / headless GPU regression tests.

**Major gaps** (from audit + inventory):

1. Decoration rendering — no underlines drawn at all (the attribute is retained), no strikethrough, no
   bold/italic faces, no minimum-contrast, no box-drawing composition, no color emoji, no ligatures.
2. Protocols — DECSCUSR / focus 1004 / sync 2026 / legacy mouse encodings / DCS (DECRQSS, XTGETTCAP) /
   OSC 8・7・133 / Kitty keyboard / Kitty graphics / grapheme clustering (2027) not implemented.
3. UX — tabs / splits / search prompt UI / URL clicking / bell / window title reflection / runtime font
   size change / fullscreen / multi-window / theme selection / background opacity not implemented.
4. Quality debt — audit items P1–P3 (modifier key encoding, combining-character loss, full copy every
   frame, no release profile configured, etc.).

Sixel is **out of parity scope**, since Ghostty itself doesn't support it. For CLI tool compatibility,
Noa instead implements it as a custom addition that reuses the existing Kitty graphics image pipeline.

---

## Phase 0 — Pay down the audit debt (P1–P3)

Rationale for going first: this phase touches the areas every later phase also touches (input.rs /
snapshot / parser / app.rs). Unless the known bugs and performance flaws there are fixed first, every
feature built on top would need rework. `.nexus/loops/noa-critical/backlog.md`'s P1–P3 is the single
source of truth.

- **P1 correctness (8 items)**: missing modifier-key encoding / combining characters & ZWJ dropped /
  CUU・CUD margin clamping / UI-thread blocking write + unbounded channel / overlong UTF-8 /
  invalid DECSTBM region / recompute on scale-factor change / redraw after Surface Lost.
- **P2 performance (5 items)**: dirty-row diffing for FrameSnapshot / eliminate CSI Vec clone /
  eliminate FontRef re-parsing / `[profile.release]` (lto, codegen-units=1) / redraw coalescing.
- **P3 (4 items)**: 8-bit ST / surface PtyEvent::Error / init-time expect() / JoinHandle, etc.

Modifier-key encoding (P1-2) is brought here to **full Ghostty-compatible parity**: Shift/Alt/Ctrl+arrow
(`CSI 1;m A`), F1–F12, Home/End/PgUp/PgDn/Insert/Delete, Alt-as-Esc prefix, and default behavior
equivalent to xterm's modifyOtherKeys. This becomes the foundation for Kitty keyboard in Phase 4.

Verification: follow-on loops using the existing verify.sh approach (`noa-p1-fidelity` / `noa-perf`).
A regression test is required for each item.

## Phase 1 — Complete VT fidelity (up through the grid)

Reach a state where "escape-sequence compatibility" with Ghostty can be declared. Everything centers on
noa-vt / noa-grid, GUI-agnostic → self-contained via unit tests.

- **DECSCUSR** (`CSI Ps SP q`): hold the 6 cursor shapes in `Terminal` state (rendering comes in Phase 2).
- **Full SGR support**: 21 double underline / 4:x underline styles (curly, etc.) / 58・59 underline
  color. Extend `CellAttrs`.
- **Modes**: 1004 focus reporting (winit Focused → CSI I/O), 2026 synchronized output (defer snapshot
  updates between BSU/ESU), 2027 grapheme clustering.
- **Combining characters & ZWJ**: switch cells to grapheme-cluster granularity (the real fix for P1-3).
  Match Ghostty's `grapheme.zig` behavior for width determination of emoji ZWJ sequences and VS16.
- **Mouse**: add X10/UTF-8(1005)/urxvt(1015) legacy encodings (currently SGR only).
- **DCS foundation + DECRQSS / XTGETTCAP**: promote the parser's DcsPassthrough to a Handler-level path.
- **BEL**: a bell event on the Handler (actually ringing it is Phase 3's UX responsibility).
- **OSC 8 / 7 / 133**: state retention on the grid side (hyperlink IDs as cell attributes, cwd and
  prompt marks on `Terminal`). UI reflection comes in Phase 3.
- **Implemented as of 2026-07-03**: DECSCUSR / DECSLRM / keypad modes / cursor style state, SGR
  21・4:x・58/59 and their decoration rendering, OSC 8/7/133 state retention, bounded DCS +
  DECRQSS/XTGETTCAP/XTVERSION/DECRQM, DECSET 1004 focus reporting, DECSET 2026 synchronized output.
- **New parity harness**: establish a thin runner that feeds esctest2 / vttest as an external CI oracle,
  plus a "same byte stream → compare screen dumps between Ghostty and noa" fixture format, in
  `tests/parity/`. Later phases' acceptance criteria are unified around "harness green."

## Phase 2 — Bring rendering up to Ghostty quality

The core of "looks like Ghostty." Centers on noa-font / noa-render.

- **Underline geometry**: single/double/curly/dotted/dashed + underline color. Composited via a
  shader/dedicated quad, same as Ghostty (using the font metrics' underline position/thickness).
  Strikethrough and overline at the same time.
- **Cursor shape rendering**: DECSCUSR's block/bar/underline + blink. A hollow outline when unfocused
  (Ghostty's behavior).
- **Bold / italic**: resolve weight/style-specific faces via font-kit, synthesizing (embolden / oblique)
  when unavailable. Add a style axis to `GlyphKey`.
- **Procedural composition of box-drawing/block elements**: render U+2500–259F, U+E0B0– (Powerline)
  without going through the font (Ghostty's `sprite/` equivalent). Also handles centering adjustments
  for Nerd Font icons within a cell.
- **Color emoji**: rasterize sbix/CBDT via swash → RGBA atlas (kept as a second atlas alongside the
  existing R8 one).
- **Shaping/ligatures**: per-line shaping via the swash shaper (ligatures like `=>`, on by default,
  configurable off). This phase's biggest structural change — generalizing the cell→glyph mapping from
  1:1 to m:n.
- **minimum-contrast**: the `minimum-contrast` setting is already exposed. CPU-side color resolution
  boosts text/underline/cursor color to meet a WCAG contrast ratio against the background.
- **Background opacity**: surface alpha + clear color alpha, driven by the `background-opacity` setting.
- Verification: extend pipeline.rs with snapshot image comparison (wgpu offscreen readback), and turn
  visual parity checks against Ghostty screenshots of the same commands into a checklist.

## Phase 3 — Scrollback foundation, links, search UI, config (≈ inc 3)

- **Paged scrollback** (implemented): replace the `VecDeque<Row>` per-row-clone approach with 64KiB
  target pages + **interned styles** (a page-local style table, equivalent to Ghostty's PageList/style
  set). Cap set by byte size rather than row count (`scrollback-limit`, default 10MB, 0 = disabled,
  page-granularity eviction). The active screen stays unpaged (scrollback is immutable after push, so no
  refcounting is needed). Column-change resizes are repacked via streaming reflow to avoid a temporary
  memory spike. `crates/noa-grid/src/scrollback.rs`.
- **OSC 8 hyperlink UI + URL auto-detection**: hover underline, Cmd+click to `open`. The regex URL
  matcher should be compatible with Ghostty's `link` setting format.
- **Search UI**: the engine is already implemented → add an overlay input prompt, match count, n/N
  navigation, Cmd+F binding.
- **Config system expansion**: move from the current 3 keys (cols/rows/font_size) toward Ghostty's config
  system: `font-family` / `font-size` / `theme` / `background-opacity` / `cursor-style` / `keybind` /
  `scrollback-limit` / `copy-on-select` / `mouse-hide-while-typing`, etc. Expose the already-implemented
  keybinding engine through config. Support reload (Cmd+Shift+,).
- **Finish small UX items**: reflect window title via OSC / bell (audio + Dock attention) /
  copy-on-select / local scrollback scrolling via wheel / runtime font size change (Cmd+±/0) /
  fullscreen.

## Phase 4 — Tabs, splits, themes (≈ inc 4)

The phase with the biggest architectural impact. Reorganizes the current structure — a single
`Arc<Mutex<Terminal>>` plus one io thread — into **Surface multiplexing** (equivalent to Ghostty's
Surface/apprt split).

- **Surface abstraction**: encapsulate {Terminal, Pty, io thread, renderer state} as a Surface, so one
  window can hold N Surfaces. Manage focus, title, and notifications per Surface.
- **Tabs**: prefer native macOS tabs (winit's tabbing identifier) for the same feel as Ghostty
  (Cmd+T/W, Cmd+1..9, Cmd+Shift+[]).
- **Splits**: a split tree (a recursive layout equivalent to Ghostty's SplitTree), Cmd+D / Cmd+Shift+D,
  focus movement (Cmd+Opt+arrow), resizing, zoom (Cmd+Shift+Enter). The renderer supports split-viewport
  rendering.
- **Multi-window**: multiple winit Windows + a per-window surface tree.
- **Theme catalog**: bundle the ~460 themes from the iTerm2-Color-Schemes set that Ghostty ships, chosen
  at build time; select via `theme = <name>` + automatic light/dark switching.
- **Font settings**: make the `font-family` fallback chain configurable, plus `font-feature` and
  `font-style` overrides.

## Phase 5 — Modern protocols (≈ inc 5)

- **Kitty keyboard protocol**: all progressive-enhancement flags (disambiguate / report events /
  alternate keys / report all keys as escapes / associated text). Built on top of Phase 0's full xterm
  implementation.
- **Kitty graphics protocol**: new APC pathway, image decoding (PNG), GPU texture management,
  placement/deletion/z-index, Unicode placeholders. Adds an image layer to the renderer. **Implemented** —
  see `docs/specs/kitty-graphics.md`.
- **Shell integration**: OSC 133 (prompt marks → prompt jump (Cmd+↑/↓), command-finish notification),
  OSC 7 (cwd → cwd inheritance for new tabs/splits, title display). Bundle integration scripts for
  zsh/bash/fish + auto-injection (equivalent to Ghostty's shell-integration).
- **Remaining DCS work**: complete XTVERSION and DECRQM responses.
- Verification: add the official kitty test suite, notcurses demos, and live `kitten icat` checks to the
  parity checklist.

## Phase 6 — macOS native polish (≈ inc 6)

- Quick Terminal (global hotkey + top-edge slide-in, equivalent to an NSPanel). **Implemented** — see the
  "Quick Terminal" section below.
- Command palette (Cmd+Shift+P, turning the already-implemented command mechanism into a list UI).
- Background blur (private API `CGSSetWindowBackgroundBlurRadius` — equivalent to Ghostty).
- Session restore (reproduce window/tab/split topology + cwd. Like Ghostty, content is not restored).
- Secure Keyboard Entry, titlebar style (`macos-titlebar-style`), Option-as-Alt setting,
  notifications (OSC 9 / 777). **Implemented**: `macos-titlebar-style` accepts
  `native`/`tabs`/`transparent`/`hidden`, `macos-option-as-alt` accepts `false`/`true`/`left`/`right`.
- CLI actions such as `noa +list-themes` / `+list-keybinds`.

### Quick Terminal (implementation notes)

A dropdown terminal that slides in from the top of the screen via a global hotkey.

**Config keys** (`noa-config`):

| Key | Type | Default | Meaning |
|---|---|---|---|
| `quick-terminal-hotkey` | string | `cmd+grave` | Global hotkey for toggling. Uses `cmd+grave`-style syntax (same notation as in-app keybindings). `grave` / `backtick` / `` ` `` are synonyms. `backslash` registers both the ANSI `\` and the JIS `¥` / `ろ` variants. `none` / `off` / an empty value skips hotkey registration. |
| `quick-terminal-size` | fraction or `%` | `0.4` | Panel height as a fraction of screen height. `0.4` or `40%`. Clamped to `0.1..=1.0`. |
| `quick-terminal-autohide` | bool | `true` | Automatically hide when focus is lost. |

**Behavior**: full screen width × `quick-terminal-size` height. Toggling produces a ~200ms slide-in/out
from the top edge (`ease_out_cubic`, driven by `about_to_wait`'s `WaitUntil` timer, interpolating window
position every frame). Also toggleable from the `View > Quick Terminal` menu and the command palette.
Excluded from session restore (automatically, by being left out of `window_order`).

**Differences from Ghostty**:
- Hotkey representation: Ghostty uses `keybind = global:<chord>=toggle_quick_terminal`. Noa uses a
  dedicated `quick-terminal-hotkey` key instead (since noa's keybindings currently can't be configured
  from a file).
- The global hotkey uses Carbon `RegisterEventHotKey` (no accessibility permission needed — this is why
  `CGEventTap` is avoided).
- The position is fixed to the top edge (Ghostty's `quick-terminal-position` top/bottom/left/right/center
  is not supported). `quick-terminal-space-behavior` is also not supported.
- Since winit cannot create a true `NSPanel`, an undecorated `NSWindow` is approximated with
  `setLevel: floating` + `collectionBehavior: canJoinAllSpaces | fullScreenAuxiliary`.
- The default size is 40% (Ghostty's default is 25%).

**Manual verification steps** (not automated, since it requires a GUI launch):
1. Add `quick-terminal-hotkey = cmd+grave` to `~/.config/noa/config` and run `cargo run -p noa`.
2. With any app in the foreground, press `Cmd+\`` → the terminal slides in from the top edge and gains
   focus.
3. Press the same hotkey again, or move focus away (autohide) → it slides out and hides.
4. Confirm it also appears on a different Space / over a fullscreen app.
5. Run `exit` in the shell inside the dropdown → the window is destroyed and recreated on the next
   toggle.

---

## Cross-cutting items

- Make the **parity harness** (introduced in Phase 1) the acceptance gate for every phase. The
  "Behavioral / Feature" dimensions of the 5-axis Parity Map become CI-checked; "Visual" becomes a
  screenshot-comparison checklist.
- **CI**: introduce GitHub Actions running `build / clippy -D warnings / test --workspace` plus a macOS
  runner, alongside Phase 0, since there is currently no CI.
- **Loop operations**: split each phase into follow-on loops under `.nexus/loops/noa-<phase>/` (AC in
  goal.md, tasks in backlog.md, a machine gate in verify.sh — same shape as noa-critical).
- **Estimated size**: Phase 0: small–medium / 1: medium / 2: large (the m:n shaping conversion is the
  main hurdle) / 3: medium–large (paging) / 4: large (Surface reorganization) / 5: large (Kitty
  graphics) / 6: medium.
- **Rationale for the ordering**: debt → semantics → visuals → foundation → major UX → protocols →
  polish. Each phase generally depends on the structural changes of the previous one (grapheme cells,
  m:n glyphs, the Surface abstraction), so reordering isn't really possible. The exception is that
  Phase 3's small UX items and Phase 2 can proceed in parallel.
