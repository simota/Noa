# Noa Feature List

Noa is a terminal emulator that faithfully reproduces the observable behavior of [Ghostty](https://ghostty.org) in Rust (macOS-first, winit + wgpu). This document is an inventory of implemented features. See [KEYBINDINGS.md](KEYBINDINGS.md) for keyboard shortcuts.

## Terminal core (noa-grid)

- **Screen grid / cursor / modes** — active region with DEC-compliant cursor clamping
- **Paged scrollback** — style-interned, byte-capped (`scrollback-limit`) page storage
- **Alt screen / DECSC・DECRC** — alternate screen switching, cursor save/restore
- **Scroll region / left-right margins** — DECSTBM + DECLRMM
- **Tab stops** — set / clear / clear all
- **Selection** — cell-range selection (word / line selection via mouse operations)
- **Interactive search** — full-text search across scrollback
- **URL detection** — hit-testing for plain-text URLs (⌘-click to open)
- **Character sets** — G0–G3 designation, locking shift (DEC Special Graphics, etc.)
- **Wide cells** — 2-cell width handling for CJK / emoji
- **Soft-wrap reflow** — reflows wrapped lines on column-count changes (coordinated with grid resize)

## VT protocol support (noa-vt + noa-grid)

A from-scratch DFA parser plus a `Handler` trait separating parsing from state.

- **Full C0 / CSI / SGR set** — 16-color + 256-color + truecolor, bold / faint / italic / inverse / invisible / strike, underline variants (single / double / curly / dotted / dashed)
- **Cursor, erase, scroll, insert/delete families** — ICH / IL / DL / DCH / ECH / SU / SD / REP, etc.
- **DA / DSR responses** — DA1 `ESC[?62;4;22c`, cursor position report, etc.
- **DEC private modes** — DECAWM, DECTCEM, DECCKM, DECNKM / DECPAM, DECLRMM, DECOM, etc.
- **Mouse tracking** — X10 / 1000 / 1002 / 1003, encodings Legacy / UTF-8 (1005) / Urxvt (1015) / SGR (1006)
- **OSC** — 0/2 (title, stack via 22/23), 7 (cwd), 8 (hyperlinks), 9/777 (notifications), 9;4 (task progress), 52 (clipboard, with policy), 4 / color family, 133 (shell integration marks)
- **Kitty graphics protocol** — image command parsing + image-layer rendering
- **Sixel graphics** — parsing of `DCS Pa;Pb;Ph q ... ST`, Sixel rasterization, rendering through the existing image layer
- **Kitty keyboard protocol** — full support for all 5 flags (disambiguate / event-types / alternate-keys / all-keys / associated-text), push / pop / set stack
- **Bracketed paste (2004) / full reset / DECSTR / DECALN**

## Windows, tabs, and splits (noa-app)

- **Multiple windows / native tabs** — new / close / select-by-number / cycle forward-backward
- **Manual tab title** — Set Tab Title prompt (from the palette / Window menu). Masks shell-originated title updates (OSC 0/2) while editing; committing an empty value clears the override. Also shown on sidebar cards (an individual card rename takes priority). Preserved across session restore (equivalent to Ghostty's `prompt_surface_title`)
- **Split tree (Splits)** — add panes left / right / up / down, max 3 panes per row/column and 9 panes overall (3x3 equivalent), directional focus movement, resizing, equalize, zoom toggle
- **Session overview** — a monitoring dashboard that live-tiles all tabs. Switch by key or click, incremental search, quick-look zoom

## UI overlays

- **Command palette** — fuzzy (subsequence) search to run actions
- **Search prompt** — incremental search UI
- **Theme & settings overlay** — a theme/settings editor with live preview opened from `Settings…` (⌘,), writes back to config
- **Sidebar (session list)** — per-window session cards, process badges, inline rename, OSC 9;4 determinate/indeterminate progress
- **Agent attention** — agent-process classification, bell-to-attention escalation, categorical status rails, one-shot arrival emphasis, Dock attention
- **About panel** — version + git hash + build date, bundled-icon resolution
- **Confirmation dialogs** — paste protection / OSC 52 / close confirmation
- **IME preedit** — underlined display of in-progress composition text

## Rendering and appearance (noa-render + noa-font)

- **wgpu instanced cell rendering** — surface-less design, minimizes lock duration via `FrameSnapshot`
- **Cursor styles** — block / bar / underline / hollow, focus / blink phase support
- **Underline rendering** — single / double / curly / dotted / dashed, hover-link underline
- **Background transparency / blur** — `background-opacity`, `background-blur-radius` (native macOS blur)
- **Background image** — `background-image` (single file / directory rotation), fit / position / repeat / opacity / interval settings
- **minimum-contrast** — enforcement of a WCAG contrast-ratio floor
- **Font pipeline** — font-kit discovery → rustybuzz shaping → swash rasterization → etagere atlas (monochrome + color emoji)
- **Ligatures / fallback** — liga / calt, CJK fallback, Nerd Font and box-drawing glyphs
- **Synthetic styles** — synthetic bold / italic, `font-thicken`
- **Themes** — bundles 574 Ghostty-compatible themes

## Configuration (noa-config)

Reads Ghostty-compatible line-oriented `key = value` syntax from `~/.config/noa/config` (with `$XDG_CONFIG_HOME` support); CLI flags override it. The legacy `config.toml` is subject to a migration warning but its contents are not read.

For the type, allowed values, defaults, and clamp/fallback rules of every key, see [CONFIGURATION.md](CONFIGURATION.md); for default keybindings and all action names, see [KEYBINDINGS.md](KEYBINDINGS.md).

| Category | Key keys |
|---|---|
| Window | `window-width/height`, `window-padding-x/-y`, `window-save-state` |
| Font | `font-family[-bold/-italic/-bold-italic]`, `font-size`, `font-feature`, `font-variation*`, `font-synthetic-style`, `font-thicken[-strength]` |
| Color/theme | `theme`, `background`, `foreground`, `cursor-color`, `selection-foreground/background`, `minimum-contrast`, `background-opacity`, `background-blur-radius` |
| Background image | `background-image`, `background-image-opacity/-position/-fit/-repeat/-interval` |
| Cursor | `cursor-style`, `cursor-style-blink`, `cursor-stop-blinking-after` |
| Bell | `visual-bell`, `audible-bell`, `audible-bell-dock-bounce`, `audible-bell-when-unfocused` |
| Behavior | `scrollback-limit`, `clipboard-read`, `clipboard-paste-protection`, `confirm-quit`, `alpha-blending`, `title-report`, `resize-overlay`, `auto-approve`, `send-selection-send-enter` |
| macOS | `macos-option-as-alt`, `macos-titlebar-style`, `macos-non-native-fullscreen`, `macos-titlebar-proxy-icon` |
| Quick Terminal | `quick-terminal-hotkey/-size/-autohide` |
| Sidebar | `sidebar-enabled/-width/-hotkey/-preview-lines` |

- **Ghostty config import** — import with migration statistics
- **Custom keybindings** — reassignment via `keybind = <chord>=<action>` / `unbind` / `clear`
- **Recognized-but-unsupported keys** — `palette` and `config-file` emit a diagnostic and their values are ignored; palette override and include are not implemented
- **Live reload** — the config file is watched and re-applied at runtime: immediately on window focus gain and settings-panel commits, otherwise on a slow 3s idle poll (so a save from inside a focused noa pane applies within ≤3s; refocus or use the settings UI to apply instantly)
- **Deviating defaults** — `cursor-stop-blinking-after = 10` (Ghostty blinks forever; `0` restores parity) and `quick-terminal-screen = mouse` (Ghostty: `main`); see CONFIGURATION.md "Deviations from Ghostty defaults" for rationale

## macOS integration

- **Native menu bar** — Noa / File / Edit / View / Window / Help. `Settings…` (⌘,) opens the theme & settings overlay
- **Fullscreen toggle** — `⌘⌃F` / View menu / command palette. Defaults to native macOS fullscreen; `macos-non-native-fullscreen = true` switches to borderless fullscreen
- **Quick Terminal** — a dropdown terminal triggered by a global hotkey (default ⌘\`), with auto-hide support
- **Secure keyboard entry** — toggleable
- **Titlebar proxy icon** — reflects the focused pane's OSC 7 pwd in the titlebar proxy icon; disabled with `macos-titlebar-proxy-icon = hidden`
- **Quick Look word lookup** — a trackpad force click (deep press) shows a dictionary popup for the word under the pointer; does not change the selection
- **Desktop notifications** — OSC 9 / 777, Dock attention
- **Clipboard** — OSC 52 (read/write policy), paste protection
- **`.app` bundle** — `scripts/bundle-macos.sh` produces an ad-hoc signed bundle
- **CLI actions** — `+version`, `+list-themes`, `+list-keybinds`, `+list-fonts`, `+show-config`, `+list-actions`, `+help`

## Session and shell integration

- **Session restore** — saves/restores window / tab / split layout
- **Shell integration** — OSC 133 (prompt/command boundaries) + OSC 7 (cwd). Bundles bash / zsh / fish scripts under `shell-integration/`
- **Jump between prompts** — ⌘↑ / ⌘↓ to scroll to the previous/next prompt

## Related documentation

- Per-feature detailed specs: `docs/specs/`
- Parity plan with Ghostty: `docs/ghostty-parity-plan.md`
- Roadmap: `docs/roadmaps/`
</content>
