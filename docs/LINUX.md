# Noa on Linux

Noa is macOS-first, but it also builds and runs on Linux under X11 and Wayland.
This guide covers building, installing, desktop integration, Linux default
keybindings, and how Linux differs from macOS. See
[CONFIGURATION.md](CONFIGURATION.md) for config keys and
[KEYBINDINGS.md](KEYBINDINGS.md) for action names.

## Requirements

- **Display server**: Wayland or X11. winit uses Wayland when
  `WAYLAND_DISPLAY` is set and falls back to X11 otherwise.
- **GPU**: a Vulkan driver (Mesa or a vendor driver), or OpenGL ES as a
  fallback.
- **Runtime libraries**: X11/Wayland and `libxkbcommon` are loaded
  dynamically.
- **Optional tools** (each feature degrades silently when its tool is missing):

  | Tool | Used for |
  |---|---|
  | `xdg-open` | Opening links and files |
  | `notify-send` (libnotify) | Desktop notifications (OSC 9 / 777) |
  | `canberra-gtk-play` | The audible bell |

## Building from source

Rust 1.92+ is required. Build dependencies (Debian/Ubuntu names):

```bash
sudo apt install build-essential pkg-config libfontconfig1-dev libfreetype-dev \
  libxkbcommon-dev libwayland-dev
```

Then:

```bash
cargo build --release -p noa
cargo run   --release -p noa -- --cols 100 --rows 30
```

## Installing

### From a release tarball

Tagged releases publish `Noa-<version>-linux-x86_64.tar.gz` (plus a
`.sha256`). The tarball unpacks to one directory laid out as an XDG prefix
(`bin/`, `share/applications/`, `share/metainfo/`, `share/icons/hicolor/`)
with an `install.sh`:

```bash
tar -xzf Noa-<version>-linux-x86_64.tar.gz -C /tmp
/tmp/Noa-<version>-linux-x86_64/install.sh                     # into ~/.local
sudo /tmp/Noa-<version>-linux-x86_64/install.sh --prefix /usr/local
/tmp/Noa-<version>-linux-x86_64/install.sh --uninstall         # same --prefix as install
```

`install.sh` copies the binary, the desktop entry, the AppStream metadata, and
the icons. It points the desktop entry's `Exec=` at the installed binary, so
the launcher works even when `<prefix>/bin` is not on the session `PATH` (the
script prints a note when it is not). It refreshes the desktop database and an
existing icon cache when those tools are present.

### Building your own tarball

```bash
scripts/package-linux.sh          # release → target/release/Noa-<ver>-linux-<arch>.tar.gz
scripts/package-linux.sh debug    # debug   → target/debug/Noa-<ver>-linux-<arch>.tar.gz
```

The packaging assets live in `dist/linux/`.

## Desktop integration

- **App id**: the Wayland app id and the X11 `WM_CLASS` are `com.simota.noa`,
  matching the desktop entry's `StartupWMClass`, so docks and task switchers
  group Noa windows under its icon.
- **Running a command**: `Noa -e <command> [args...]` runs a command instead of
  the shell. The desktop entry declares `X-TerminalArgExec=-e` for launchers
  that open programs in a terminal.
- **Clipboard**: `ctrl+shift+c` / `ctrl+shift+v` use the CLIPBOARD selection.
  The PRIMARY selection is supported: `copy-on-select` fills it, and
  middle-click pastes it.
- **Links and files**: Ctrl-click a URL, an OSC 8 hyperlink, or a file path to
  open it (with `xdg-open`, or the configured editor for files), as in
  Ghostty's GTK build.
- **Notifications and bell**: desktop notifications go through `notify-send`.
  The bell plays the sound theme's `bell` event through `canberra-gtk-play` and
  also marks the window as urgent.
- **Fonts**: fonts come from fontconfig. If `font-family` is unset, Noa uses
  the first installed of JetBrains Mono, DejaVu Sans Mono, Noto Sans Mono,
  Ubuntu Mono, and Liberation Mono, then the generic `monospace` alias.
  Fontconfig fallback covers CJK, emoji, and symbols.
- **Config**: Noa reads `$XDG_CONFIG_HOME/noa/config`, or
  `~/.config/noa/config` when `XDG_CONFIG_HOME` is unset.

## Tabs and the context menu

winit has no native tab or popup-menu API off macOS, so the renderer draws both
inside the window:

- **Tabs**: New Tab (`ctrl+shift+t`) adds a tab to the current window rather
  than opening a new one. A tab bar appears once a window has two or more tabs.
  Click a tab to select it, or click its close button to close it.
- **Right-click menu**: right-click a pane to open it. Its items are:
  - Copy, Paste, Clear
  - Add Pane Left/Right/Up/Down, Equalize Splits, Toggle Split Zoom
  - Auto Approve
  - Send Selection to Pane
  - Set Tab Title…, New Tab, Close Tab

  To choose an item, hover and click it, or use the arrow keys and press Enter.
  Escape closes the menu. So do a click outside it, the mouse wheel, focus
  loss, and a window resize. The menu does not open while a dialog, prompt, or
  palette is showing.

## Default keybindings

Linux follows Ghostty's GTK defaults:

- App commands use **Ctrl+Shift**, so plain Ctrl chords stay with the shell.
- Super is mostly left to the desktop; only split resizing uses it.

Every binding can be changed with `keybind = …` (see
[KEYBINDINGS.md](KEYBINDINGS.md)).

| Chord | Action |
|---|---|
| `ctrl+shift+t` / `ctrl+shift+n` | New tab / new window |
| `ctrl+shift+w` / `alt+f4` | Close tab / close window |
| `ctrl+shift+q` | Quit |
| `alt+1` … `alt+9` | Select tab 1–9 |
| `ctrl+tab` / `ctrl+shift+tab`, `ctrl+pagedown` / `ctrl+pageup` | Next / previous tab |
| `ctrl+shift+c` / `ctrl+shift+v` (also `ctrl+insert` / `shift+insert`) | Copy / paste |
| `ctrl+shift+a` | Select all |
| `ctrl+shift+k` | Clear |
| `ctrl+shift+o` / `ctrl+shift+e` | Split right / split down |
| `ctrl+alt+arrow` | Focus the split in that direction |
| `super+ctrl+shift+arrow` / `super+ctrl+shift+plus` | Resize split / equalize splits |
| `ctrl+shift+enter` | Toggle split zoom |
| `ctrl+enter` | Toggle fullscreen |
| `ctrl+shift+f` / `ctrl+shift+g` / `ctrl+shift+h` | Find / find next / find previous |
| `shift+pageup` / `shift+pagedown`, `shift+home` / `shift+end` | Scroll by page / to top or bottom |
| `ctrl+shift+pageup` / `ctrl+shift+pagedown` | Jump to previous / next prompt |
| `ctrl+=` / `ctrl+-` / `ctrl+0` | Font size up / down / reset |
| `ctrl+shift+p` | Command palette |
| `ctrl+shift+s` | Sidebar |
| `ctrl+shift+m` | Send selection to pane |
| `ctrl+shift+j` | Next notification |
| `ctrl+shift+x` | Scratch terminal (`ctrl+shift+t` is New Tab here) |
| `ctrl+,` / `ctrl+shift+,` | Preferences / reload config |

## Differences from macOS

- `quick-terminal-hotkey` is not registered system-wide, because Wayland gives
  applications no global-shortcut API. A `toggle_quick_terminal` keybinding
  still works while a Noa window is focused.
- These macOS-only integrations are absent:
  - the menu bar and Ember
  - AppleScript
  - Dock badges
  - background blur
  - the native AppKit overlay cards (the GPU renderer draws the command palette
    and dialogs instead)
- File previews open in your editor instead of a native text panel.
- Encrypted scrollback persistence needs the macOS Keychain, so restored
  sessions come back without their scrollback.

## Troubleshooting

- **Logs**: run from a terminal with `RUST_LOG=info Noa` (or `debug`). Noa logs
  through `env_logger`.
- **Window does not appear / GPU errors**: check that a Vulkan driver is
  installed (`vulkaninfo --summary` from `vulkan-tools`). Without one, Noa
  falls back to OpenGL ES.
- **Forcing X11 on a Wayland session**: `env -u WAYLAND_DISPLAY Noa` runs Noa
  through XWayland.
- **The shell exits right away**: run `cargo run -p noa-pty --example probe` to
  check pty and shell startup.
- **The launcher entry does nothing**: reinstall with `install.sh` so that
  `Exec=` points at the installed binary. Then check
  `desktop-file-validate ~/.local/share/applications/com.simota.noa.desktop`.
- **No notifications or bell sound**: install `libnotify-bin` (`notify-send`)
  and `gnome-session-canberra` or `libcanberra-gtk3-module`
  (`canberra-gtk-play`; the package name varies by distribution).
