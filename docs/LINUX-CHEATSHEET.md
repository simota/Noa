# Noa Linux Shortcut Cheat Sheet

Default shortcuts on Linux (X11 and Wayland). App commands use **Ctrl+Shift**,
so plain Ctrl chords stay with the shell. Every binding can be changed with
`keybind = <chord>=<action>`; run `noa +list-keybinds` to print the active
table. See [LINUX.md](LINUX.md) for the Linux guide and
[KEYBINDINGS.md](KEYBINDINGS.md) for action names and chord syntax.

## Windows and tabs

| Key | Action |
|---|---|
| `Ctrl+Shift+N` | New window |
| `Ctrl+Shift+T` | New tab |
| `Ctrl+Shift+W` | Close pane (the tab when unsplit) |
| `Alt+F4` | Close window |
| `Ctrl+Shift+Q` | Quit |
| `Alt+1` … `Alt+9` | Select tab 1–9 |
| `Ctrl+Tab` / `Ctrl+PageDown` | Next tab |
| `Ctrl+Shift+Tab` / `Ctrl+PageUp` | Previous tab |
| `Ctrl+Shift+PageUp` / `Ctrl+Shift+PageDown` | Move the tab left / right |
| `Ctrl+Enter` | Toggle fullscreen |

## Splits

| Key | Action |
|---|---|
| `Ctrl+Shift+O` | Split right |
| `Ctrl+Shift+E` | Split down |
| `Ctrl+Alt+←` / `→` / `↑` / `↓` | Focus the split in that direction |
| `Super+Ctrl+Shift+←` / `→` / `↑` / `↓` | Resize the split |
| `Super+Ctrl+Shift++` | Equalize splits |
| `Ctrl+Shift+Enter` | Toggle split zoom |

Split left / up have no default key; use the command palette or the
right-click menu.

## Copy, paste, and selection

| Key | Action |
|---|---|
| `Ctrl+Shift+C` / `Ctrl+Insert` | Copy |
| `Ctrl+Shift+V` / `Shift+Insert` | Paste |
| `Ctrl+Shift+A` | Select all |
| `Ctrl+Shift+M` | Send selection to pane |
| `Shift+←` / `→` / `↑` / `↓` | Enter copy mode and extend the selection |

In copy mode: arrows move, `Shift`+arrows extend, `Enter` copies and exits,
`Escape` clears the selection, then exits. The `Shift`+arrow gestures pass
through to the running program on the alternate screen.

## Search

| Key | Action |
|---|---|
| `Ctrl+Shift+F` | Open the search prompt (again to close) |
| `Ctrl+Shift+G` | Find next |
| `Ctrl+Shift+H` | Find previous |
| `Enter` / `Shift+Enter` | Next / previous match (prompt open) |
| `Escape` | Close the prompt and clear the query |

The prompt accepts Emacs-style editing: `Ctrl+A` / `E` (start / end),
`Ctrl+B` / `F` (left / right), `Ctrl+H` / `D` (delete back / forward),
`Ctrl+W` (delete word), `Ctrl+U` / `K` (delete to start / end).

## Scrolling

| Key | Action |
|---|---|
| `Shift+PageUp` / `Shift+PageDown` | Scroll one page |
| `Shift+Home` / `Shift+End` | Jump to top / bottom |
| `Ctrl+Shift+↑` / `Ctrl+Shift+↓` | Previous / next prompt (needs shell integration) |

## Terminal and font

| Key | Action |
|---|---|
| `Ctrl+Shift+K` | Clear screen |
| `Ctrl+=` / `Ctrl++` | Increase font size |
| `Ctrl+-` | Decrease font size |
| `Ctrl+0` | Reset font size |

## Panels and launchers

| Key | Action |
|---|---|
| `Ctrl+Shift+P` | Command palette |
| `Ctrl+Shift+S` | Sidebar |
| `Ctrl+Shift+L` | Session overview |
| `Ctrl+Shift+D` | Scratch terminal |
| `Ctrl+Shift+B` | Quick terminal (only while a Noa window is focused) |
| `Ctrl+Shift+J` | Next unread notification |
| `Ctrl+,` | Preferences |
| `Ctrl+Shift+,` | Reload config |

Inside the palette, overview, and right-click menu: arrows move, `Enter`
chooses, `Escape` closes. In the session overview, `Tab` toggles quick-look
zoom.

## Mouse

| Gesture | Action |
|---|---|
| `Ctrl` + hover / click | Underline / open a link or file path |
| `Shift` + click / drag / wheel | Select or scroll locally, bypassing mouse tracking |
| Double-click / triple-click | Select word / line |
| Right-click | Open the pane context menu |

## Differs from macOS

| Action | macOS | Linux |
|---|---|---|
| Split right / down | `⌘D` / `⌘⇧D` | `Ctrl+Shift+O` / `Ctrl+Shift+E` |
| Session overview | `⌘⇧O` | `Ctrl+Shift+L` |
| Scratch terminal | `⌘⇧T` | `Ctrl+Shift+D` |
| Quick terminal | `` ⌘` `` (system-wide) | `Ctrl+Shift+B` (in-app only) |
| Select tab N | `⌘N` | `Alt+N` |
| Find next / previous | `⌘G` / `⌘⇧G` | `Ctrl+Shift+G` / `Ctrl+Shift+H` |
| Fullscreen | `⌘⌃F` | `Ctrl+Enter` |

Source of truth: the non-macOS `default_specs()` in
`crates/noa-app/src/commands/keybind.rs`.
