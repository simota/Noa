# Noa Keyboard Shortcut Reference

A complete reference of the keyboard shortcuts Noa handles (excludes
shell-side keys). The default bindings are implemented at
`KeybindEngine::default()` in `crates/noa-app/src/commands/keybind.rs`.
Config `keybind =` entries are applied on top of this default table in
order. The list of active bindings can also be checked from the CLI:

```bash
noa +list-keybinds
```

## Config `keybind =`

`keybind = <chord>=<action>` adds to or overrides the default table.
The same chord takes the later entry. `keybind = <chord>=unbind` clears
that chord, and `keybind = clear` clears all bindings defined before it.

```text
keybind = cmd+i=prompt_surface_title
keybind = cmd+t=unbind
keybind = cmd+shift+n=tab.new
```

`<chord>` is `+`-separated. Modifier aliases are `cmd`/`command`/`super`/`meta`,
`ctrl`/`control`, `alt`/`option`, `shift`. Keys accept a single character,
`plus`, arrow keys such as `arrowup`/`up` (short aliases allowed),
`pageup`, `pagedown`, `home`, `end`, `enter`/`return`, and
`grave`/`backtick` (`` ` ``).

`<action>` uses a name from the "canonical action list" below.
`noa +list-keybinds` only shows currently active bindings, so actions
that are unbound by default aren't printed. Some Ghostty-style action
names — such as `new_tab`, `prompt_surface_title`,
`toggle_quick_terminal` — are also accepted as compatible input.

### Canonical Action List

| Category | Action |
|---|---|
| App | `about`, `preferences`, `config.reload`, `app.quit` |
| Edit | `copy`, `paste`, `pane.send-selection` |
| Terminal | `terminal.clear`, `terminal.clear-scrollback`, `terminal.select-all`, `terminal.export-scrollback`, `terminal.pipe-scrollback-to-pager` |
| Font | `font-size.increase`, `font-size.decrease`, `font-size.reset` |
| Search | `search.find`, `search.next`, `search.previous`, `search.clear` |
| Scroll | `scroll.line-up`, `scroll.line-down`, `scroll.page-up`, `scroll.page-down`, `scroll.top`, `scroll.bottom`, `scroll.prev-prompt`, `scroll.next-prompt` |
| Tab | `tab.new`, `tab.close`, `tab.next`, `tab.previous`, `tab.set-title`, `tab.select-1` … `tab.select-9` |
| Window | `window.new`, `window.close`, `fullscreen.toggle` |
| Split | `split.new-left`, `split.new-right`, `split.new-up`, `split.new-down`, `split.focus-left`, `split.focus-right`, `split.focus-up`, `split.focus-down`, `split.resize-left`, `split.resize-right`, `split.resize-up`, `split.resize-down`, `split.equalize`, `split.toggle-zoom` |
| UI | `session-overview.toggle`, `command-palette.toggle`, `quick-terminal.toggle`, `secure-keyboard-entry.toggle`, `sidebar.toggle`, `auto-approve.toggle`, `theme-settings.open` |

`tab-overview.toggle` is also accepted as a compatible name for
`session-overview.toggle`. If the input contains `_`, the name with `-`
substituted is also matched. The full Ghostty-style alias table is
sourced from `ghostty_action_alias` in
`crates/noa-app/src/commands/keybind.rs`.

## Global (While Terminal Is Focused)

### App / Window / Tab

| Key | Action |
|---|---|
| ⌘Q | Quit |
| ⌘T | New tab |
| ⌘N | New window |
| ⌘W | Close tab |
| ⌘⇧W | Close window |
| ⌘⌃F | Toggle fullscreen |
| ⌘1 – ⌘9 | Select tab 1-9 |
| ⌘⇧] | Next tab |
| ⌘⇧[ | Previous tab |

### Splits

| Key | Action |
|---|---|
| ⌘D | Add pane to the right |
| ⌘⇧D | Add pane below |
| ⌘⌃← / → / ↑ / ↓ | Move split focus |
| ⌘⌥← / → / ↑ / ↓ | Move split focus (alias) |
| ⌘⌃⇧← / → / ↑ / ↓ | Resize split |
| ⌘⌃= | Equalize splits |
| ⌘⇧Enter | Toggle split zoom |

Add Pane Left / Add Pane Up have no default keybinding. They can be run
from the command palette or the right-click context menu. Panes can be
added up to 3 per row/column, up to 9 panes per tab maximum. Adding a
pane past the limit is a no-op. In the command palette and right-click
context menu, Add Pane directions that can no longer be created are
disabled. Split actions have no menu entry and are only reachable via
keybindings and the right-click context menu (Add Pane Left / Add Pane
Right / Add Pane Up / Add Pane Down / Equalize Splits / Toggle Split
Zoom).

### Edit / Terminal / Font

| Key | Action |
|---|---|
| ⌘C | Copy |
| ⌘V | Paste |
| ⌘⇧M | Send selection to pane |
| ⌘A | Select all |
| ⌘K | Clear screen |
| ⌘= / ⌘⇧+ | Increase font size |
| ⌘- | Decrease font size |
| ⌘0 | Reset font size |

### Search

| Key | Action |
|---|---|
| ⌘F | Open search prompt |
| ⌘G | Find next |
| ⌘⇧G | Find previous |

⌘⇧F is intentionally left unassigned for future use.

### Scroll (Viewport Manipulation, Not Sent to pty)

| Key | Action |
|---|---|
| ⇧↑ / ⇧↓ | Scroll 1 line |
| ⇧PageUp / ⇧PageDown | Scroll 1 page |
| ⇧Home / ⇧End | Jump to top / bottom |
| ⌘↑ / ⌘↓ | Jump to previous / next prompt (requires shell integration OSC 133) |

Shift-only scroll does not trigger if another modifier is also held.

### Overlay Launchers

| Key | Action |
|---|---|
| ⌘⇧O | Toggle Session Overview (tab overview) |
| ⌘⇧P | Toggle command palette |
| ⌘⇧S | Toggle sidebar |

Actions with no default keybinding can also be run from the command
palette / menu. Notable ones include Reload Configuration, Clear
Scrollback, Toggle Quick Terminal, Secure Keyboard Entry, About, Open
Preferences, Open Theme & Settings, Export Scrollback, Pipe Scrollback
to Pager, Toggle Auto Approve, Set Tab Title.

> Unbound ⌘-combination keys are swallowed and never leak to the pty.

## Global System Hotkeys

System-wide hotkeys via Carbon `RegisterEventHotKey`. These fire even
when the app isn't focused. Configurable via config.

| Config key | Default | Action |
|---|---|---|
| `quick-terminal-hotkey` | `cmd+grave` (⌘`) | Toggle Quick Terminal |
| `sidebar-hotkey` | none (disabled) | Toggle sidebar |

The syntax is a `+`-separated chord (e.g. `cmd+shift+t`). Modifier
aliases: `cmd`/`command`/`super`/`meta`, `ctrl`/`control`, `alt`/`option`,
`shift`. Keys accept letters, digits, and the following tokens.

- Symbols: `=`/`equal`, `-`/`minus`, `[`/`leftbracket`, `]`/`rightbracket`,
  `;`/`semicolon`, `,`/`comma`, `.`/`period`, `/`/`slash`
- Basic keys: `enter`/`return`, `tab`, `space`, `escape`/`esc`
- Backtick: `grave`, `backtick`, `` ` ``
- Backslash: `backslash` or `\`. Registers both ANSI `\` and JIS `¥` / `ろ`
  simultaneously
- JIS-specific: `yen`/`jis-yen`/`intl-yen`,
  `underscore`/`jis-underscore`/`intl-ro` (aliases for `_` and `-` also work)

Unlike in-app `keybind`, global hotkeys don't accept arrow keys,
`PageUp` / `PageDown`, or `Home` / `End`. A hotkey can be disabled with
`none` / `off` / `false` / an empty value.

## Key Handling Within Overlays

Each overlay is modal — while it's shown, key input never reaches the
pty.

### Search Prompt (⌘F)

| Key | Behavior |
|---|---|
| Escape | Close and clear the query |
| Enter / ⇧Enter | Move to next / previous match while staying open |
| ⌘G / ⌘⇧G | Next / previous while staying open |
| ⌘F (press again) | Close (keeps highlight and active match) |
| Backspace | Delete 1 character |
| Printable characters | Append to query |

### Command Palette (⌘⇧P)

| Key | Behavior |
|---|---|
| Escape | Close without executing |
| Enter | Run the selected command |
| ↑ / ↓ | Move selection |
| ⌘⇧P | Close (toggle) |
| Printable characters | Append to query (subsequence filtering) |

### Session Overview (⌘⇧O)

| Key | Behavior |
|---|---|
| ← / → / ↑ / ↓ | Move tile selection |
| Enter | Open the selected tab |
| Escape | Two stages: clears the search query if one exists, otherwise closes |
| Tab | Toggle quick-look zoom |
| ⌘1 – ⌘9 | Switch directly to a tab |
| Printable characters | Append to search query |

### Confirmation Dialogs (Paste Protection / OSC 52 / Close Confirmation)

| Key | Behavior |
|---|---|
| Enter / y | Confirm / execute |
| Escape / n | Cancel |

### Sidebar Inline Rename

| Key | Behavior |
|---|---|
| Enter | Confirm (empty string is treated as cancel) |
| Escape | Cancel |

## Mouse + Modifiers

| Action | Behavior |
|---|---|
| ⇧ + click / drag / wheel | Bypasses mouse tracking mode for local selection / scroll |
| ⌘ + hover | Pointer + underline over a link (OSC 8 / auto-detected URL) |
| ⌘ + left click | Open the hovered link |
| Left double-click | Select word |
| Left triple-click | Select line |
| Right click | Focus the pane and show the split context menu |

## Primary Sources

- `crates/noa-app/src/commands/keybind.rs` — `KeybindEngine`, default bindings, config application (source of truth)
- `crates/noa-app/src/commands/command.rs` — `AppCommand`, action name conversion
- `crates/noa-app/src/commands/key_token.rs` — chord parser, key aliases
- `crates/noa-app/src/commands.rs` — facade / re-export of the above modules
- `crates/noa-app/src/macos_menu.rs` — menu accelerators + context menu
- `crates/noa-app/src/app/event_loop.rs` — key / mouse routing
- `crates/noa-app/src/app/input_ops.rs` — search prompt / command palette / confirmation dialogs
- `crates/noa-app/src/macos_hotkey.rs` — global hotkeys
- `docs/CONFIGURATION.md` — complete reference of config keys, values, and defaults
