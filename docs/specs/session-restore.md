# Session restore

Ghostty-parity Phase 6 item: persist and restore the window / tab / split
**topology** and each pane's **cwd** across launches. Terminal *contents* are
not restored by this feature (matching Ghostty).

Restoring the layout but not the output is a promise the layout makes and the
contents break, so the two are wired together deliberately: a pane restored
with no record shows a one-line notice saying its output was not saved and
naming the key that would change that. Contents are opt-in through the
separate `scrollback-persist` key — see
[`scrollback-persistence.md`](scrollback-persistence.md), which also owns the
`scrollback` field this document's leaf schema carries.

## `window-save-state`

New `noa-config` scalar key, Ghostty-compatible values `default | never | always`.

- `default` and `always` both save on exit and restore on launch. noa has no
  OS-level "reopen on relaunch" signal to defer to, so **`default` is treated as
  `always`** (`WindowSaveState::restores()` returns `true` for both).
- `never` disables both saving and restoring.
- Default when the key is absent: `default` (i.e. restore is on).

Surfaced by `+show-config` and accepted by the Ghostty config importer
(`is_supported_scalar_key`).

## Persisted file

`<data-dir>/noa/session.json` (`noa-config::session_state_path`); on macOS
`<data-dir>` is `~/Library/Application Support`. Written atomically (temp file +
rename). A versioned, hand-written JSON document (`SESSION_VERSION = 3`; the
crate has no serde, matching the hand-written config parser). Version 2 added
per-leaf `remote` metadata, version 3 the per-leaf `scrollback` snapshot key.
Schema:

```json
{
  "version": 1,
  "focused_window": 0,
  "windows": [
    {
      "frame": { "x": 100, "y": 50, "width": 800, "height": 600 },
      "focused_tab": 0,
      "tabs": [
        { "focused_leaf": 0,
          "split": { "type": "split", "orientation": "horizontal", "ratio": 0.5,
                     "first":  { "type": "leaf", "cwd": "/a",
                                 "remote": null, "scrollback": "3f1c8a02b7d94e56" },
                     "second": { "type": "leaf", "cwd": null,
                                 "remote": null, "scrollback": null } } }
      ]
    }
  ]
}
```

- A **window** is one AppKit tab group; its `frame` is logical (scale-independent)
  pixels, position optional.
- A **tab** carries its recursive split tree; `focused_leaf` indexes the leaves
  in pre-order.
- A **leaf**'s `cwd` is the OSC 7 cwd (8635cdb) when it still resolves to a local
  directory, else `null` (the pane then opens in the process cwd).
- A **leaf**'s `scrollback` names its persisted snapshot under
  `<data-dir>/noa/scrollback/`, or `null` when `scrollback-persist` is off or
  the pane has nothing saved. Keys are validated as 16 lowercase hex digits on
  read: this file is user-writable and the key is interpolated into a path.

A missing, unreadable, malformed, or version-mismatched file parses to "no
session" — startup is **never** blocked by session state.

## Save triggers

`persist_session()` writes the live topology on every structural change (new
tab/window, close tab, split, close pane) and on clean quit (`exiting`). It is a
no-op while restoring, when `window-save-state = never`, and **when no windows
are live** — the last case deliberately leaves the previously written file
intact, so the close-last-window path still restores that final window next
launch, and a crash restores the most recent structural state.

## Restore

On the first `resumed`, when enabled and **no explicit `--cols`/`--rows` was
passed** (those suppress restore so the requested size wins), the saved topology
is rebuilt: one AppKit tab group per window, its tabs spawned in order, each
tab's split tree materialized by reusing the initial pane as the first leaf and
spawning a fresh pane (with its saved cwd) for every other leaf. Frames, focused
tab, and focused pane are reapplied. The file is left in place after restore
(crash resilience). If restore yields no window, a fresh default tab is spawned.

## Manual verification

1. `cargo run -p noa`; open a couple of tabs (`cmd+t`), split a tab
   (`cmd+d` / `cmd+shift+d`), `cd` into different directories in several panes.
2. Quit (`cmd+q`), relaunch `cargo run -p noa`: the same windows/tabs/splits
   reappear, each pane's shell starting in its former cwd.
3. `printf 'window-save-state = never\n' >> "$(…)/noa/config"`, relaunch: a
   single fresh window (no restore); the file stops updating.
4. Corrupt `session.json` (e.g. `echo x > session.json`), relaunch: normal fresh
   start, no error.
