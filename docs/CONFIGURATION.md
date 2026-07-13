# Noa Configuration Reference

This document is a reference for the config keys, values, and defaults accepted by the current `noa-config` implementation.
See [KEYBINDINGS.md](KEYBINDINGS.md) for the default keybinding table and action names.

## Load location and format

Noa reads `$XDG_CONFIG_HOME/noa/config` at startup. If `XDG_CONFIG_HOME` is unset, it falls back to
`~/.config/noa/config`. The old `config.toml` is not read for content — only a migration warning is shown.

The format is Ghostty-compatible line-oriented `key = value`. Blank lines and lines starting with `#`
(after leading whitespace is stripped) are ignored. The entire value can be wrapped in double quotes, but
escape sequences and trailing comments are not interpreted.

```conf
# Window size is measured in terminal cells.
window-width = 100
window-height = 30
font-family = "Fira Code"
font-size = 15
theme = "Catppuccin Mocha"
```

If the same scalar key appears more than once, the last occurrence wins. `font-family*`,
`font-feature`, `font-variation*`, and `keybind` are repeatable and accumulate in the order they appear.
CLI options take precedence over the config file.

You can inspect the currently resolved configuration with:

```bash
noa +show-config
```

At present, `+show-config` does not print `background-image*` or `resize-overlay`, so check those
values directly in the config file.

## Window and session

| Key | Accepted values | Default | Description |
|---|---|---|---|
| `window-width` | integer `0..=65535` | `80` | Number of columns. Must be specified together with `window-height` in the config; rounded up to a minimum of `10` at resolution time |
| `window-height` | integer `0..=65535` | `24` | Number of rows. Must be specified together with `window-width` in the config; rounded up to a minimum of `4` at resolution time |
| `window-padding-x` | finite decimal `>= 0` | unspecified | Left/right padding. When unspecified, defaults to `24` physical px on the left and `16` on the right |
| `window-padding-y` | finite decimal `>= 0` | unspecified | Top/bottom padding. When unspecified, defaults to `0` physical px on top and `16` on the bottom |
| `window-save-state` | `default`, `never`, `always` | `default` | `default` and `always` save/restore; `never` disables it |
| `confirm-quit` | `true`, `false` | `true` | Confirm before quitting the app |
| `resize-overlay` | `after-first`, `always`, `never` | `after-first` | Display of `cols × rows` during resize. `after-first` excludes only the initial layout |

If only one of `window-width` or `window-height` is set in the config, both are ignored and a diagnostic
is shown. The CLI flags `--cols` / `--rows` can each be specified independently.

## Fonts

| Key | Accepted values | Default | Description |
|---|---|---|---|
| `font-size` | finite decimal `> 0` | `14` | Font size |
| `font-family` | non-empty family name | unspecified | Priority order for the regular face. When unspecified, uses a platform fallback that prefers macOS's `Menlo` |
| `font-family-bold` | non-empty family name | unspecified | Priority order for the bold-specific family |
| `font-family-italic` | non-empty family name | unspecified | Priority order for the italic-specific family |
| `font-family-bold-italic` | non-empty family name | unspecified | Priority order for the bold-italic-specific family |
| `font-feature` | 4-character ASCII tag, or `-` + tag | none | e.g. `calt`, `liga`, `-dlig`. Repeatable |
| `font-variation` | `<4-char ASCII axis>=<finite decimal>` | none | e.g. `wght=650`. Repeatable |
| `font-variation-bold` | same as above | none | Variable-font axis for bold |
| `font-variation-italic` | same as above | none | Variable-font axis for italic |
| `font-variation-bold-italic` | same as above | none | Variable-font axis for bold italic |
| `font-synthetic-style` | `true`, `false`, `no-bold`, `no-italic` | equivalent to `true` | Whether synthetic bold/italic is allowed |
| `font-thicken` | `true`, `false` | `true` | Stem thickening of glyphs |
| `font-thicken-strength` | integer `0..=255` | `255` | Thickening strength. `0` has no effect |
| `alpha-blending` | `native`, `linear`, `linear-corrected` | `native` | `linear` variants are recognized but show a diagnostic and currently fall back to `native` |

The family, feature, and variation keys can each be written on multiple lines. Example for the regular face:

```conf
font-family = Fira Code
font-family = Menlo
font-feature = calt
font-feature = -dlig
font-variation = wght=550
```

## Theme, colors, cursor

| Key | Accepted values | Default | Description |
|---|---|---|---|
| `theme` | one bundled theme name | unspecified | Name of a `.conf` file under `crates/noa-theme/vendor/themes/`, minus the extension. Paired `light:...` / `dark:...` specification is not yet supported |
| `background` | `#RRGGBB` or `RRGGBB` | theme value | Background color override |
| `foreground` | `#RRGGBB` or `RRGGBB` | theme value | Foreground color override |
| `cursor-color` | `#RRGGBB` or `RRGGBB` | theme value | Cursor color override |
| `selection-foreground` | `#RRGGBB` or `RRGGBB` | theme value | Selected text color override |
| `selection-background` | `#RRGGBB` or `RRGGBB` | theme value | Selected background color override |
| `minimum-contrast` | finite decimal `1.0..=21.0` | `1.0` | Lower bound of the WCAG contrast ratio. `1.0` means no correction |
| `cursor-style` | `block`, `bar`, `underline` | blinking block | `block_hollow` is recognized but ignored as unsupported |
| `cursor-style-blink` | `true`, `false` | equivalent to `true` | Cursor blinking. It also blinks when only the shape is specified |
| `background-opacity` | finite decimal | `1.0` | Clamped to `0.0..=1.0` |
| `background-blur-radius` | `true`, `false`, non-negative integer | `0` | macOS blur. `true` maps to `20`, `false` to `0`; integers are clamped to `0..=64` |

The list of themes can be inspected with `noa +list-themes`.

## Background image

| Key | Accepted values | Default | Description |
|---|---|---|---|
| `background-image` | `noa`, a PNG file path, or a directory path | none | `noa` selects the bundled wallpaper; paths expand `~`. For a directory, the PNG files directly inside it are rotated in name order |
| `background-image-opacity` | finite decimal | `1.0` | Clamped to `0.0..=1.0`. Independent of window opacity |
| `background-image-position` | `top-left`, `top-center`, `top-right`, `center-left`, `center`, `center-right`, `bottom-left`, `bottom-center`, `bottom-right` | `center` | Placement or crop anchor |
| `background-image-fit` | `none`, `contain`, `cover`, `stretch` | `contain` | Scaling method |
| `background-image-repeat` | `true`, `false` | `false` | Tile the image |
| `background-image-interval` | positive integer seconds | `30` | Interval for switching within a directory. Values `1..=4` are rounded up to `5` seconds |

An unspecified or empty `background-image` shows no background image. Only an exact match of `noa`
uses the bundled wallpaper. Only PNG image decoding is supported. If the file is missing, is not a PNG,
or fails to decode, a diagnostic is shown and the background image is disabled.

## Terminal, clipboard, bell

| Key | Accepted values | Default | Description |
|---|---|---|---|
| `scrollback-limit` | integer `>= 0` | `10000000` | Total byte count for scrollback. `0` disables it |
| `clipboard-read` | `deny` / `false`, `ask`, `allow` / `true` | `ask` | Policy for OSC 52 clipboard read |
| `clipboard-paste-protection` | `true`, `false` | `true` | Confirmation for pastes that could trigger command execution |
| `title-report` | `true`, `false` | `false` | Allow window title responses via `CSI 21 t` |
| `visual-bell` | `true`, `false` | `false` | Flash the window on BEL |
| `audible-bell` | `true`, `false` | `false` | Play a platform sound on BEL |
| `audible-bell-when-unfocused` | `true`, `false` | `false` | Only sound the audible bell when unfocused |
| `audible-bell-dock-bounce` | `true`, `false` | `false` | Trigger Dock attention on an unfocused audible BEL. macOS only |
| `auto-approve` | `true`, `false` | `false` | Initial value for agent CLI auto approval in new tabs |
| `send-selection-send-enter` | `true`, `false` | `false` | Send Enter after the send-selection picker pastes into the target pane |

## Quick Terminal and sidebar

| Key | Accepted values | Default | Description |
|---|---|---|---|
| `quick-terminal-hotkey` | global hotkey chord, or `none` / `off` / `false` | `cmd+grave` | System-wide hotkey for the Quick Terminal. An empty value also disables it |
| `quick-terminal-size` | positive finite decimal, or percentage | `0.4` | Ratio relative to screen height. Clamped to `0.1..=1.0`. e.g. `40%` |
| `quick-terminal-autohide` | `true`, `false` | `true` | Automatically hide when focus is lost |
| `sidebar-enabled` | `true`, `false` | `false` | Initial sidebar visibility for new windows |
| `sidebar-width` | finite decimal `200..=600` | `360` | Sidebar width (points) |
| `sidebar-font-size` | finite decimal `8..=20` | `11.5` | Session sidebar font size (points) |
| `sidebar-hotkey` | global hotkey chord, or `none` / `off` / `false` | none | System-wide hotkey for the sidebar. An empty value also disables it |
| `sidebar-preview-lines` | integer `0..=20` | `5` | Number of trailing lines shown in a card. `0` means no preview |

See [KEYBINDINGS.md](KEYBINDINGS.md#global-system-hotkeys) for the syntax of global hotkey chords and
the corresponding keys.

## macOS

| Key | Accepted values | Default | Description |
|---|---|---|---|
| `macos-option-as-alt` | `false` / `none`, `true` / `both`, `left` / `only-left`, `right` / `only-right` | `false` | Scope in which the Option key is treated as terminal Alt |
| `macos-titlebar-style` | `native` / `tabs`, `transparent` | `native` | Titlebar style for regular terminal windows |
| `macos-non-native-fullscreen` | `true`, `false` | `false` | Use borderless fullscreen instead of a native fullscreen Space |
| `macos-titlebar-proxy-icon` | `visible` / `true`, `hidden` / `false` | `visible` | Whether to show the focused pane's OSC 7 pwd as a proxy icon in the titlebar |

On non-macOS platforms, macOS-specific display and window behaviors are no-ops.

## Keybindings

`keybind` is repeatable, and entries are applied to the default bindings in order from top to bottom.

```conf
keybind = cmd+i=tab.set-title
keybind = cmd+t=unbind
keybind = clear
keybind = cmd+shift+n=tab.new
```

- `keybind = <chord>=<action>`: adds or overrides a chord
- `keybind = <chord>=unbind`: unbinds a chord
- `keybind = clear`: removes all default and added bindings up to that point

See [KEYBINDINGS.md](KEYBINDINGS.md) for chord syntax, the full list of canonical actions, and the default bindings.

## Recognized but unsupported keys

| Key | Current behavior |
|---|---|
| `palette` | Shows a diagnostic and is ignored. Palette override is not implemented |
| `config-file` | Shows a diagnostic and is ignored. Config include is not implemented |

Unknown keys and invalid values show a diagnostic, and that override is not applied.

## Importing a Ghostty config

`noa --import-ghostty-config` reads Ghostty's candidate config and copies the lines that Noa recognizes
as importable into `$XDG_CONFIG_HOME/noa/config`. If the target file already exists, it is not overwritten.
Unsupported lines are not removed — they are commented out with `# `.
