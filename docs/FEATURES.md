# Noa Feature List

Noa is a terminal emulator that faithfully reproduces the observable behavior of [Ghostty](https://ghostty.org) in Rust (macOS-first, winit + wgpu). This document is an inventory of implemented features. See [KEYBINDINGS.md](KEYBINDINGS.md) for keyboard shortcuts.

## Terminal core (noa-grid)

- **Screen grid / cursor / modes**: active region with DEC-compliant cursor clamping
- **Paged scrollback**: style-interned, byte-capped (`scrollback-limit-bytes`) page storage, compressed while idle (`scrollback-compression`)
- **Alt screen / DECSC・DECRC**: alternate screen switching, cursor save/restore
- **Scroll region / left-right margins**: DECSTBM + DECLRMM
- **Tab stops**: set / clear / clear all
- **Selection**: cell-range selection (word / line selection via mouse operations)
- **Interactive search**: full-text search across scrollback
- **URL detection**: hit-testing for plain-text URLs (⌘-click to open)
- **Character sets**: G0–G3 designation, locking shift (DEC Special Graphics, etc.)
- **Wide cells**: 2-cell width handling for CJK / emoji
- **Soft-wrap reflow**: reflows wrapped lines on column-count changes (coordinated with grid resize)

## VT protocol support (noa-vt + noa-grid)

A from-scratch DFA parser plus a `Handler` trait separating parsing from state.

- **Full C0 / CSI / SGR set**: 16-color + 256-color + truecolor, bold / faint / italic / inverse / invisible / strike, underline variants (single / double / curly / dotted / dashed)
- **Cursor, erase, scroll, insert/delete families**: ICH / IL / DL / DCH / ECH / SU / SD / REP, etc.
- **DA / DSR responses**: DA1 `ESC[?62;4;22c`, cursor position report (CPR, and DECXCPR `CSI ? 6 n` with page 1), DECRQCRA screen checksums (opt-in, `vt-xt-checksum-report`), etc.
- **DEC private modes**: DECAWM, DECTCEM, DECCKM, DECNKM / DECPAM, DECBKM, DECLRMM, DECOM, etc.
- **ANSI modes**: IRM (4), LNM (20); KAM (2) and SRM (12) are tracked for DECRQM only. DECRQM reports the unimplemented ANSI modes 1, 5, 7, 10, 11 and 13–19, and DECECM (`?117`), as permanently reset
- **Mouse tracking**: X10 / 1000 / 1002 / 1003, encodings Legacy / UTF-8 (1005) / Urxvt (1015) / SGR (1006)
- **OSC**: 0/2 (title, stack via 22/23), 7 (cwd), 8 (hyperlinks), 9/777 (notifications), 9;4 (task progress), 52 (clipboard, with policy), 5522 (Kitty clipboard protocol: multi-type read/write, paste events via mode 5522), 4 / color family, 133 (shell integration marks; `click_events` prompt clicks)
- **Kitty graphics protocol**: image command parsing + image-layer rendering
- **Glyph Protocol** (APC `25a1`): answers `s` / `q` / `r` / `c` like Ghostty 1.4.0: validates `glyf` registrations (simple glyphs only, no hinting, 64 KiB) into a per-terminal glossary of up to 1024 Private Use Area codepoints, cleared by RIS. Registered glyphs are not drawn (Ghostty 1.4.0 doesn't either)
- **Sixel graphics**: parsing of `DCS Pa;Pb;Ph q ... ST`, Sixel rasterization, rendering through the existing image layer
- **Legacy key encoding**: Ghostty 1.4.0's table: modified Backspace / Tab / Enter / Escape send `CSI 27 ; mods ; code ~` (except Alt alone, which ESC-prefixes, and Shift+Tab / Shift+Enter: `CSI Z` / `ESC CR`), xterm `modifyOtherKeys` level 2 (`CSI > 4 ; 2 m`), Alt ESC-prefixes the whole text, keypad keys keep their character (or `ESC O <mods> <final>` under DECKPAM once mode 1035, on by default as in Ghostty, is reset) whatever the modifiers
- **Kitty keyboard protocol**: full support for all 5 flags (disambiguate / event-types / alternate-keys / all-keys / associated-text), push / pop / set stack
- **Bracketed paste (2004) / full reset / DECSTR / DECALN**

## Windows, tabs, and splits (noa-app)

- **Multiple windows / native tabs**: new / close / select-by-number / cycle forward-backward
- **Manual tab title**: Set Tab Title prompt (from the palette / Window menu). Masks shell-originated title updates (OSC 0/2) while editing; committing an empty value clears the override. Also shown on sidebar cards (an individual card rename takes priority). Preserved across session restore (equivalent to Ghostty's `prompt_surface_title`)
- **Split tree (Splits)**: add panes left / right / up / down, max 3 panes per row/column and 9 panes overall (3x3 equivalent), directional focus movement, resizing, equalize, zoom toggle (animated: clip reveal, 400 ms; instant under Reduce Motion), zoom icon in the tab title
- **Session overview**: a monitoring dashboard that live-tiles all tabs. Switch by key or click, incremental search, quick-look zoom

## UI overlays

- **Idle Ember**: opt in with `ember-idle-after` (default off): a silent, display-only companion in the focused main pane, with fade-in/out, a 4.8-second play, and a question emote. Gentle gestures run every 20 seconds for up to 10 minutes of resting; macOS Reduce Motion uses a still pose without gestures.
- **Command palette**: fuzzy (subsequence) search to run actions; the initials of a title's words match too (`nt` → New Tab), and entries whose description matches (as a substring or by initials, like Ghostty 1.4.0) are listed below the title matches
- **Search prompt**: incremental search UI. On macOS the query is shared through the system find pasteboard, like Ghostty 1.4.0: a new prompt starts from the text another app's find bar left there, and each edit publishes the query
- **Theme & settings overlay**: a theme/settings editor with live preview opened from `Settings…` (⌘,), writes back to config
- **Sidebar (session list)**: per-window session cards, process badges, inline rename, OSC 9;4 determinate/indeterminate progress
- **Agent attention**: agent-process classification, bell-to-attention escalation, categorical status rails, one-shot arrival emphasis, Dock attention
- **Agent workflows**: pane-scoped notifications, next-unread navigation (⌘⇧J), explicit agent state and a Claude Code hook adapter, native multiline prompt drafts, and read-only output snapshots with search/code copying; see [Agent workflows](AGENT_WORKFLOW.md)
- **Agent orchestration**: `Noa +ctl` spawns worker panes running Claude Code, Codex, agy, or any command, sends them prompts once they are ready, waits on their reported state, reads their output and the report file each worker is given, and closes them, keeping a per-orchestrator journal that `+ctl status` reads back; `+ctl retry` starts a worker whose result failed the check again in its directory, worktree and branch, on the same or another model tier, with its task and a note that the previous attempt's changes are there; `Noa +ctl board` shows the workers in a pane, as a queue of those that need someone above the tree of work under way, with the selected worker's screen previewed, and sends to, focuses or closes them; the first spawn opens the board, and workers live in floating panes stowed on it, opened over the board one at a time; noa-server panels carry agent state, exit status, and parent/label. See [Agent workflows](AGENT_WORKFLOW.md#orchestrating-worker-panes)
- **Local file navigation**: configurable editor line/column navigation on ⌘-click; UTF-8 file preview on ⌘⌥-click
- **About panel**: version + git hash + build date, bundled-icon resolution
- **Confirmation dialogs**: paste protection / clipboard read and write (OSC 52, OSC 5522, with "allow and remember") / close confirmation, with "Review Windows" when quitting or closing a window leaves several tabs' programs running
- **IME preedit**: underlined display of in-progress composition text

## Rendering and appearance (noa-render + noa-font)

- **wgpu instanced cell rendering**: surface-less design, minimizes lock duration via `FrameSnapshot`
- **Cursor styles**: block / bar / underline / hollow, focus / blink phase support
- **Underline rendering**: single / double / curly / dotted / dashed, hover-link underline
- **Background transparency / blur**: `background-opacity`, `background-blur-radius` (native macOS blur)
- **Glassmorphism mode**: `glassmorphism` (`off`/`1`–`5`, default `off`, higher = more transparent): translucent frosted sidebar / session-overview / titlebar-tab-bar chrome and frosted command palette / prompts / dialogs over the blurred desktop; the rim takes over drawing each pane's edge as the faces thin out, reaching the foreground color at `5`. Any on-level takes over `background-opacity` / `background-blur-radius` with that level's own recommended pair
- **Background image**: `background-image` (single file / directory rotation), fit / position / repeat / opacity / interval settings
- **minimum-contrast**: enforcement of a WCAG contrast-ratio floor
- **Font pipeline**: font-kit discovery → rustybuzz shaping → swash rasterization → etagere atlas (monochrome + color emoji)
- **Emoji presentation**: VS16 selects color emoji, VS15 prefers text glyphs, and color artwork fits its actual cell width and line height. Skin tones, ZWJ families/professions, flags, and keycaps compose in ordinary shell and agent output, including across PTY reads. Grapheme mode 2027 defaults on; applications can query it with `CSI ? 2027 $ p` or disable it with `CSI ? 2027 l` for scalar-width compatibility. A selector arriving at the last column keeps the existing one-cell footprint and the artwork fits that space.
- **Ligatures / fallback**: liga / calt, CJK fallback, Nerd Font and box-drawing glyphs
- **Synthetic styles**: synthetic bold / italic, `font-thicken`
- **Themes**: bundles 574 Ghostty-compatible themes plus `Noa`, built from the brand palette (charcoal, Ember white, one orange accent on the cursor and ANSI yellow; not the default)

## Configuration (noa-config)

Reads Ghostty-compatible line-oriented `key = value` syntax from `~/.config/noa/config` (with `$XDG_CONFIG_HOME` support); CLI flags override it. The legacy `config.toml` is subject to a migration warning but its contents are not read.

For the type, allowed values, defaults, and clamp/fallback rules of every key, see [CONFIGURATION.md](CONFIGURATION.md); for default keybindings and all action names, see [KEYBINDINGS.md](KEYBINDINGS.md).

| Category | Key keys |
|---|---|
| Window | `window-width/height`, `window-padding-x/-y`, `window-save-state` |
| Font | `font-family[-bold/-italic/-bold-italic]`, `font-size`, `font-feature`, `font-variation*`, `font-synthetic-style`, `font-thicken[-strength]` |
| Color/theme | `theme`, `background`, `foreground`, `cursor-color`, `selection-foreground/background`, `minimum-contrast`, `background-opacity`, `background-blur-radius`, `glassmorphism` |
| Background image | `background-image`, `background-image-opacity/-position/-fit/-repeat/-interval` |
| Cursor | `cursor-style`, `cursor-style-blink`, `cursor-stop-blinking-after` |
| Bell | `visual-bell`, `audible-bell`, `audible-bell-dock-bounce`, `audible-bell-when-unfocused` |
| Behavior | `scrollback-limit-bytes` (`scrollback-limit`), `scrollback-limit-lines`, `scrollback-compression`, `cursor-click-to-move`, `vt-window-resize-allowed`, `vt-xt-checksum-report`, `vt-xt-checksum-extension`, `clipboard-read`, `clipboard-write`, `clipboard-write-limit-bytes`, `copy-on-select`, `right-click-action`, `middle-click-action`, `clipboard-paste-protection`, `confirm-quit`, `alpha-blending`, `title-report`, `link-osc8`, `resize-overlay`, `auto-approve`, `send-selection-send-enter`, `ember` |
| macOS | `macos-option-as-alt`, `macos-titlebar-style`, `macos-non-native-fullscreen`, `macos-titlebar-proxy-icon` |
| Quick Terminal | `quick-terminal-hotkey/-size/-autohide` |
| Scratch Terminal | `scratch-terminal-key/-size` |
| Sidebar | `sidebar-enabled/-width/-hotkey/-preview-lines` |

- **Ghostty config import**: import with migration statistics
- **Custom keybindings**: reassignment via `keybind = <chord>=<action>` / `unbind` / `clear`
- **`spawn:` action** (Noa extension): `keybind` and `command-palette-entry` can run `spawn:<command line>` in the background through a login shell, in the focused pane's directory
- **Palette overrides and includes**: `palette = N=#RRGGBB` overrides one palette entry; `config-file` includes another file (`?` makes it optional)
- **Live reload**: the config file is watched and re-applied at runtime: immediately on window focus gain and settings-panel commits, otherwise on a slow 3s idle poll (so a save from inside a focused noa pane applies within ≤3s; refocus or use the settings UI to apply instantly)
- **Deviating defaults**: `cursor-stop-blinking-after = 10` (Ghostty blinks forever; `0` restores parity) and `quick-terminal-screen = mouse` (Ghostty: `main`); see CONFIGURATION.md "Deviations from Ghostty defaults" for rationale

## macOS integration

- **Native menu bar**: Noa / File / Edit / View / Window / Help. `Settings…` (⌘,) opens the theme & settings overlay
- **Fullscreen toggle**: `⌘⌃F` / View menu / command palette. Defaults to native macOS fullscreen; `macos-non-native-fullscreen = true` switches to borderless fullscreen
- **Quick Terminal**: a dropdown terminal triggered by a global hotkey (default ⌘\`), with auto-hide support
- **Scratch Terminal**: a disposable popup terminal (default ⌘⇧T, in-app only) centered on the focused window, opened in the focused pane's cwd; closes on toggle/focus-loss/shell-exit/⌘W, with no persistence
- **Floating panes**: terminals that float above the split layout and stay until closed (default ⌘⇧Y; ⌘⇧I moves focus between the last float and the split pane). Drag the title strip to move, an edge or corner to resize; the strip's buttons collapse a float to its strip, maximize it over its anchor, and close it. The strip's icon switches the anchor between the pane (clipped to it, hidden and closed with it), the tab, and the window (stays across tab switches). The strip shows the agent's state and turns to the attention color when it needs you. Saved with the session when `window-save-state` is on; spec in [`docs/specs/floating-panes.md`](specs/floating-panes.md)
- **Secure keyboard entry**: toggleable
- **Ember**: Noa's ghost in the menu bar: every pane ordered `waiting.` → `running…` → `idle` → `done.`, with the waiting count beside the ghost; pick a row to jump there. View → Ember in Menu Bar, or `ember = true` at launch
- **Agent Dashboard**: View → Agent Dashboard (or the command palette's Open Agent Dashboard) starts the agent dashboard (`widgets/agent-dashboard`) as its own window. It reads Noa's panes over the control server, so `server-enable = true` is needed; its Open button also needs `control` in `server-scopes`: the details drawer says so before anything is pressed, and a refused Open says why on the notice line at the window's foot, which leaves the header's link state readable. The needs-you queue is split into 判断待ち (approvals, then questions), 要確認 (failures) and 参考情報 (notices and quiet panes), the last left out of its count and folded away behind a title-row button. A selected worker that a live update takes off the screen is deselected rather than replaced by its neighbour, so Enter reaches only a pane chosen. The drawer shows up to 40 lines of a worker's screen and, when asked, the start of its result file (`$NOA_RESULT`, regular files only, 16 KiB / 200 lines), with buttons to copy the screen tail and to copy or reload the result. The drawer also sends a message to the worker (pasted, then submitted with Enter), which needs `input` in `server-scopes`. Nothing is sent to a pane waiting for a permission: the dashboard refuses it, and the dashboard checks again just before sending. A message that was not delivered comes back as the worker's draft, and drafts are kept per worker until sent; a restarted Noa drops them. The drawer can also close the worker's pane (`control`), which a second press confirms; any other action, or 8 s, takes the first press back. Clicking a header urgency count filters by it. Besides the operations panels (運用), tabs show statistics (統計: fleet KPIs, phase time, output throughput, per agent/model, wait ranking), a map (マップ: workers as a tree by orchestrator, branch or state, with a warning for several active workers on one branch) and a timeline (タイムライン: running and human-wait concurrency over a Gantt view of the last 15 or 60 minutes or all of it); they follow live updates and keep their scroll positions. `--tab=` and `--filter=` set the initial tab and filter. When the dashboard cannot be found or started, or exits with an error, Noa shows why in a toast. `scripts/bundle-macos.sh` ships it in `Noa.app/Contents/Helpers`; a development build uses `widgets/agent-dashboard/target`. Keyboard: `J`/`K` or arrows select a worker (Enter opens its pane, Space opens a details drawer for any worker), `N` jumps through the waiting workers in queue order, `S` writes to the selected worker, `X` closes its pane (twice), `G` cycles the grouping, `T` cycles the tabs, `/` filters by label, agent, model, branch or task, with `field:value` qualifiers (`state:`, `need:`, `agent:`, `model:`, `branch:`, `parent:`, `label:`, `task:`, `result:`) and `-` to negate a term, `?` lists every shortcut, 1–5 toggle panels. The header shows link health (dot, time since sync, stale/disconnected marked); panels that overflow page by whole cards and say how many are hidden, except that a relation-graph row (a parent with its workers) taller than its panel is shown alone and cut at the bottom, its cut workers not counted
- **Repo Browser**: View → Repo Browser (or the command palette's Open Repo Browser) starts the repo browser (`widgets/repo-browser`) as its own window: every repository Noa's panes are in with all of its worktrees from `git worktree list` (a worktree no pane is in, such as a finished worker's, is marked ペインなし and can be reviewed but not sent to or focused), their changed files (Unstaged, Staged, or vs `main`/`master`), files changed in more than one worktree of a repository marked as conflicts, the files `git merge-tree` (git 2.38 or later) predicts would conflict when a worktree's branch is merged into the base, marked ⛔ on the worktree, the file, the diff header and the status bar, and the selected file's diff, with old and new line numbers, beside the worktree's pane. A "コミット" tab in the right panel lists the worktree's commits beyond `main`/`master` (newest first, up to 50, with short sha, subject, author, age and file count); choosing one (or its "コミット: …" command palette entry) shows its full `git show` in the diff view, with line numbers, find and quoting (a quote is headed by the commit's short sha), and choosing a file again returns to the file diff. Selected diff lines (⌘L or the quote button) are appended to one review prompt as blocks headed by their line range, so remarks on several hunks and files go out together; a repeated quote is ignored, a quote from another worktree is refused (one prompt holds one worktree's diffs and is sent only from that worktree), and Clear empties the prompt (pressed again as Restore it brings the text back once; restoring over another worktree's prompt swaps the two). The prompt goes to the worktree's primary pane unless another of its panes is picked in the review tab; the pick holds while that worktree stays selected and the pane exists, and the live terminal tab follows it. Focusing and dropped files (sent as shell-escaped paths) use the same pane. The toolbar's split button (and the command palette, ⌘T / ⌘E) opens a new focused Noa tab in the selected worktree's root, which works for a worktree without panes: "ここでシェルを開く" starts the shell there, and "エディタで開く" runs the editor on the selected file at the first selected diff line's new-side number (else the first hunk's start, else line 1). The editor is `$VISUAL`, else `$EDITOR`, else `vi`, started through the login shell (`$SHELL -lc`, bash/zsh syntax) so the shell's own settings apply; VS Code style editors (`code`, `cursor`, `codium`) get `--goto path:LINE`, all others `+LINE path`. The shell needs `control` and the editor also `input` in `server-scopes`. The filter takes `no-pane` and `merge-conflict` tokens. It reads Noa's panes over the control server, so `server-enable = true` is needed; focusing a pane needs `control` and sending needs `input` in `server-scopes`. When it cannot be found or started, or exits with an error, Noa shows why in a toast. `scripts/bundle-macos.sh` ships it in `Noa.app/Contents/Helpers`; a development build uses `widgets/repo-browser/target`.
- **Discussion**: View → Discussion (or the command palette's Open Discussion) starts the discussion (`widgets/discussion`) as its own window: a moderator agent ("Noa": claude, codex or agy) relays a topic to panelist agents (claude, codex, agy), and every exchange goes through the moderator, who summarizes each round. A discussion runs at most its round limit: every brief counts down the rounds left, and the last round ends in the moderator's conclusion on the topic, decided by the moderator where the panel did not agree (a conclusion given earlier ends it sooner); when it clearly cannot converge, the moderator may lower the limit or raise it up to twice what the user set. Each discussion opens a Noa tab of its own with the moderator and every panelist running interactively in splits. Every round, each panelist is sent a situation brief the widget builds from the discussion's state (topic, issue tree with every stance, what changed, the full text of every message since its last brief, interventions, its question, where to write its reply); a brief is resent when its agent does not start or reply in time, or reports an error, and shown as failed after the retries. Agents write their replies to files under the session's directory (`<data dir>/noa/discussions/<id>/`, with the event log `events.jsonl` a session is resumed from). It drives Noa over the control server, so `server-enable = true` and `read,control,input` in `server-scopes` are needed; a lost connection is retried with backoff. When it cannot be found or started, or exits with an error, Noa shows why in a toast. `scripts/bundle-macos.sh` ships it in `Noa.app/Contents/Helpers`; a development build uses `widgets/discussion/target`.
- **Titlebar proxy icon**: reflects the focused pane's OSC 7 pwd in the titlebar proxy icon; disabled with `macos-titlebar-proxy-icon = hidden`
- **Quick Look word lookup**: a trackpad force click (deep press) shows a dictionary popup for the word under the pointer; does not change the selection
- **Desktop notifications**: OSC 9 / 777, Dock attention
- **Clipboard**: OSC 52 and Kitty OSC 5522 (read/write policy, `clipboard-write-limit-bytes`), paste protection
- **`.app` bundle**: `scripts/bundle-macos.sh` produces an ad-hoc signed bundle
- **CLI actions**: `+version`, `+list-themes`, `+list-keybinds`, `+list-fonts`, `+show-config`, `+explain-config`, `+list-actions`, `+help`

## Session and shell integration

- **Session restore**: saves/restores window / tab / split layout
- **Shell integration**: OSC 133 (prompt/command boundaries) + OSC 7 (cwd). Bundles bash / zsh / fish scripts under `shell-integration/`
- **Jump between prompts**: ⌘↑ / ⌘↓ to scroll to the previous/next prompt
- **Child exit notice**: a process that fails within `abnormal-command-exit-runtime` (250 ms) keeps its pane open with an error, like Ghostty 1.4.0: a red bar along the pane's bottom edge on macOS, a message in the terminal elsewhere. Any key closes the pane
- **`NOA_SURFACE_ID`**: every pane's child gets a random nonzero 64-bit id as `0x` plus 16 hex digits, like Ghostty's `GHOSTTY_SURFACE_ID`
- **`NOA_QUICK_TERMINAL`**: set to `1` for every pane in the Quick Terminal, its first pane and its splits, like Ghostty's `GHOSTTY_QUICK_TERMINAL`

## Related documentation

- Per-feature detailed specs: `docs/specs/`
- Parity plan with Ghostty: `docs/ghostty-parity-plan.md`
- Roadmap: `docs/roadmaps/`
</content>
