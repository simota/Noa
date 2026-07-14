# noa-server Operations Runbook

Target: `noa-ipc` — a JSON-RPC 2.0 over WebSocket server (spec: `docs/specs/noa-server.md`).
Lets a client connect to a running noa instance to list panels, fetch text/grid, perform operations, send input, subscribe in real time, or attach another Noa instance to a pane's raw VT stream.

## 1. Enabling it

**Fully disabled by default** (no port is opened at all). In `~/.config/noa/config`
(or `$XDG_CONFIG_HOME/noa/config`):

```
server-enable = true
# The following can be omitted (default values)
server-port = 61771
server-bind = 127.0.0.1
server-scopes = read
```

| Key | Type / default | Meaning |
|------|----------------|------|
| `server-enable` | bool / `false` | Server startup gate (FR-1) |
| `server-port` | u16 / `61771` | Bind port (FR-2) |
| `server-bind` | IP address string / `127.0.0.1` | Interface to bind. Loopback-only by default. Specifying e.g. `0.0.0.0` allows direct reachability from other hosts on the LAN (v2 opt-in; see "LAN exposure procedure" at the end of this section for details) |
| `server-token` | string / none | Explicit auth token override. When set, the token file is neither generated nor read |
| `server-scopes` | csv / `read` | Upper bound on scopes that can be granted. A subset of `read,control,input,attach`. `control` (focus/tab/split/close), `input` (sendText), and `attach` (interactive raw VT attach) can only be granted when **explicitly listed** |

Restart and confirm it's enabled:

```sh
lsof -nP -iTCP:61771 -sTCP:LISTEN   # OK if noa is LISTENing on 127.0.0.1:61771
```

On a bind failure (e.g. port conflict), the app does not crash — it only logs a warning:
`noa-ipc: failed to bind <server-bind>:<port>: <err>`.

Instead of editing the config file directly, `server-enable`/`server-port`/`server-bind`/`server-scopes`
can also be controlled from the Settings panel (opened via `⌘,` or similar, Settings mode). The row names
in the panel are "Server", "Server Port", "Server Bind" (right after Server Port), and "Server Scopes",
all applied via Save (ON SAVE badge) — once saved, `ConfigWatcher` detects the file change and
restarts/stops/rebinds the server within 1 second. The "Server Bind" row toggles between the two values
`127.0.0.1` and `0.0.0.0` with `←`/`→` (if the config was hand-edited to a value that is neither of these
two, that value is shown as-is, and the next ←/→ lands on one of the two values). The `server-token`
value itself is intentionally not exposed in the panel since it's a secret (the "Server Token" row in §2
is copy-only and never displays the value).

Right after the "Server" row is a read-only "Server Status" row showing the server's current state as one
of three forms: `Running (<bind>:<port>, <N> client(s))` / `Stopped` / `Bind failed: <reason>`
(editing/Reset disabled, not subject to Save, always the LIVE badge). Because the panel's render path
cannot directly reference `App`'s live state, this row is written back explicitly every time `App`
re-runs `install_ipc_server_if_needed`/`restart_ipc_server` while a session is open (including when it
detects a config reload change to `server-enable`/`server-port`/`server-bind`/`server-scopes`) — from
saving a toggle in the panel to the actual display update, this lands within one cycle of
`ConfigWatcher`'s 500ms poll.

### LAN exposure procedure

Steps to opt into widening the default loopback-only bind so other hosts on the same LAN can reach it
directly. **There is no TLS, so only use this on a trusted network (e.g. your home LAN)**. On an
untrusted network (public Wi-Fi, a shared office network, etc.), continue to reach it via an SSH port
forward, Tailscale, or a similar tunnel as before — token auth alone does not protect the plaintext
communication itself. When using a tunnel, the network between endpoints is assumed to be protected
by that tunnel.

1. Add `server-bind = 0.0.0.0` (or the IP of a specific interface) to `~/.config/noa/config` (equivalent
   to toggling it via `←`/`→` on the "Server Bind" row in the Settings panel).
2. Restart noa, or if `server-enable` is already true, it will auto-rebind within 1 second of saving the
   config.
3. Confirm the bind:
   ```sh
   lsof -nP -iTCP:61771 -sTCP:LISTEN   # if ADDRESS is *:61771, the 0.0.0.0 bind succeeded
   ```
4. Connect from a client using the LAN IP (use the same token as for loopback):
   ```sh
   websocat ws://<LAN-IP>:61771/
   ```
5. When no longer needed, remove `server-bind` (or set it back to `127.0.0.1`) to revert to the default.



## 2. Token

- If `server-token` is unset, one is auto-generated on first startup: **`~/.config/noa/server-token`**
  (permissions 0600, 64 hex characters).
- If a file with permissions looser than 0600 is detected, it is auto-repaired (chmod 0600 + warning log).
- Rotation (v1): delete the file and restart noa → a new token is generated. Connected clients are
  unaffected (authentication happens only at connection establishment).

```sh
TOKEN=$(cat ~/.config/noa/server-token)
```

The "Server Token" row (right after Server Scopes) in the Settings panel (Settings mode) copies the
token currently in use — the `server-token` value if set, otherwise the file above (auto-generated if
not yet created) — to the clipboard each time you press `←`/`→` or `Enter`. The value itself is never
shown on screen; the row display cycles through three states only: "Copy to clipboard" → "Copied ✓"
(or "Copy failed" on failure). Since it's an action row with no value, it is not subject to Save and
always shows the LIVE badge (completes the instant you press it, no save needed).

## 3. Connection and handshake

Two authentication methods (either one):
1. An `Authorization: Bearer <token>` header at WS upgrade time
2. `params.token` in `noa.hello` right after connecting

Either way, **`noa.hello` must come first** (other methods return `-32001`). `protocolVersion` is
currently `1` (a major version mismatch returns `-32006`).

```sh
# Example interaction with websocat (brew install websocat)
websocat ws://127.0.0.1:61771/
{"jsonrpc":"2.0","id":1,"method":"noa.hello","params":{"protocolVersion":1,"token":"<TOKEN>","scopes":["read","control","input","attach"]}}
# → {"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"grantedScopes":["read"],"serverVersion":"0.1.2"}}
```

`grantedScopes` = requested scopes ∩ `server-scopes`. With the default settings, requesting
`control`/`input`/`attach` still only returns `["read"]` (AC-20).

## 4. Method quick reference

IDs (`windowGroupId`/`windowId`/`paneId`) are always **decimal strings**.

```json
{"jsonrpc":"2.0","id":2,"method":"noa.listPanels","params":{}}
{"jsonrpc":"2.0","id":3,"method":"noa.getText","params":{"paneId":"1","source":"scrollback","maxBytes":65536}}
{"jsonrpc":"2.0","id":4,"method":"noa.getGrid","params":{"paneId":"1","startRow":0,"rowCount":50}}
{"jsonrpc":"2.0","id":5,"method":"noa.sendText","params":{"paneId":"1","text":"ls\n"}}
{"jsonrpc":"2.0","id":6,"method":"noa.focusPane","params":{"paneId":"1"}}
{"jsonrpc":"2.0","id":7,"method":"noa.newTab","params":{"windowId":"..."}}
{"jsonrpc":"2.0","id":8,"method":"noa.split","params":{"paneId":"1","direction":"horizontal"}}
{"jsonrpc":"2.0","id":9,"method":"noa.closePane","params":{"paneId":"1"}}
{"jsonrpc":"2.0","id":10,"method":"noa.subscribe","params":{"events":["state_changed","output"],"paneIds":["1"]}}
{"jsonrpc":"2.0","id":11,"method":"noa.unsubscribe","params":{"subscriptionId":"..."}}
{"jsonrpc":"2.0","id":12,"method":"noa.attach","params":{"paneId":"1"}}
{"jsonrpc":"2.0","id":13,"method":"noa.resizePane","params":{"paneId":"1","cols":120,"rows":40}}
{"jsonrpc":"2.0","id":14,"method":"noa.detach","params":{"paneId":"1"}}
```

Notes:
- `getText`'s `source`: `screen` = visible screen only / `scrollback` = the full scrollback + visible
  screen. Exceeding `maxBytes` (default 256KiB, server-side cap clamped to 1MiB) truncates
  **preferring the tail**, with `truncated:true`.
- `getGrid`: row 0 is the absolute coordinate of the oldest scrollback row. Max 2048 rows per request +
  256KiB response cap. If truncated, `hasMore:true` — advance `startRow` to fetch the rest.
- `sendText`'s `paste` (optional, default `true`): `false` injects raw text without bracketed-paste
  wrapping. To send a bare Enter, use `{"text":"\r","paste":false}`.
- `newTab`'s `windowId`: resolves against either a native window id or a window group id. If omitted,
  defaults to the active window.
- `split`'s `direction`: `horizontal` = left/right split / `vertical` = top/bottom split.
- `attach` returns a fixed `ws://<host>:<port>/attach` URL and a 10-second one-time token. Open the
  second WebSocket, send the token as its first binary frame, then treat every data frame as raw VT
  bytes. Never put the token in a URL, log, or persisted session file. `detach` only tears down the
  subscription; it does not terminate the pane's process.
- Notifications: `noa.stateChanged` (only changed panels) / `noa.output` (a color-run diff of only the
  changed lines, coalesced at ≥16ms). On subscription channel overflow, the oldest notifications are
  dropped, and the next notification carries `dropped:true`.

## 5. Error codes

| code | Meaning | Primary remedy |
|------|------|---------|
| `-32001` | Auth failure / method called before hello | Verify the token; send `noa.hello` first |
| `-32002` | Unknown paneId/windowId | Re-fetch with `noa.listPanels` (panels disappear when closed) |
| `-32003` | Insufficient scope | Add the required scope to `server-scopes`, restart noa, and request it via hello |
| `-32004` | Panel disappeared mid-execution | No retry needed, target is gone |
| `-32005` | Payload too large | Lower `maxBytes`/`rowCount` |
| `-32006` | protocolVersion major mismatch | Update the client |
| `-32007` | Pane already has a reserved or active attach | Detach the existing client or choose another pane |
| `-32008` | Raw attach handshake failed | Request a fresh one-time token and retry; never reuse the failed token |
| `-32601` | Unknown method | Connection is kept alive (additive-only compatible behavior) |

## 6. Operational notes

- **Exposure scope**: bound to `127.0.0.1` only by default. `server-bind` (e.g. `0.0.0.0`) allows
  opting into a direct LAN bind (v2, see "LAN exposure procedure" at the start of this section) — but
  note that there is still no TLS, so communication remains plaintext. For remote access from an
  untrusted network (e.g. iOS), continue to reach it via an SSH port forward / Tailscale / similar
  tunnel rather than a LAN bind.
- **Mutating scopes are opt-in**: minimize the permissions of tokens handed to automation agents via
  `server-scopes` (keep `read` only if just browsing).
- **Mutation timeouts**: focus/newTab/split/close/sendText execute via a main-thread round trip and time
  out after 2 seconds (Internal error). **They may still execute with delay after the timeout**
  (at-least-once). Blindly retrying on error can result in double execution.
- **Performance**: structured `noa.output` notifications use bounded try-send + drop-oldest; a stalled
  structured client receives `dropped:true`. Raw attach output must never drop bytes, so it uses a
  separate byte-counted 1MiB queue and waits outside the Terminal lock. If backpressure persists for
  2 seconds, only that raw attach disconnects and the client re-attaches with a fresh seed.
- **Connection limits**: 32 concurrent WebSockets, including both JSON-RPC control connections and raw
  attach connections. Excess connections are closed immediately. 1MiB per
  message / 256KiB per frame cap. 16 subscriptions per connection (exceeding `noa.subscribe` returns
  `-32005` without closing the connection).
- **Effect of config reload**: changes to `server-enable`/`server-port`/`server-bind`/`server-token`/
  `server-scopes` take effect immediately on config file rewrite (500ms polling) — the server restarts
  (existing connections self-terminate within ~50ms) and rebinds with the new settings. If disabled, it
  simply does not start. Other (non-server) keys do not trigger a restart. The broadcaster is held once
  for the app's lifetime and reused across server restarts, so output pushes for panes spawned before a
  restart still reach `noa.output` subscribers after the restart (no need to reopen the pane).
  **The same applies when enabling `server-enable` after the server had been disabled the whole time**:
  already-open panes keep their output-push tap even while disabled (gating is by subscriber presence,
  not tap presence), so once enabled, a client subscribing to `output` via `noa.subscribe` starts
  receiving delivery immediately without reopening the pane.
- **Quick Terminal is excluded**: for the same reason it's excluded from the sidebar, Quick Terminal
  panes never appear in `noa.listPanels` or in output pushes (an intentional v1 spec).
- **closePane skips confirmation**: `noa.closePane` treats the `control` scope as authorized automation
  and closes the pane immediately without the GUI confirmation dialog, even if a process is running
  (unlike the confirmation dialog shown by normal operations like cmd+w). Passing the wrong pane id
  closes it — and any running process — without confirmation, so validate the target id on the
  automation side before calling it.

## 7. Troubleshooting

| Symptom | Check |
|------|------|
| Port doesn't open | Is `server-enable` true? / does the log show `noa-ipc: failed to bind`? / `lsof -iTCP:61771` |
| Connection closes immediately | Message/frame size cap exceeded (1MiB/256KiB), connection count exceeds 32, or `noa.hello` wasn't completed within 10 seconds of connecting (the handshake itself has a 5-second deadline) |
| All methods return -32001 | Is `noa.hello` sent first? / does the token match the file? (if `server-token` is set, it takes priority) |
| sendText returns -32003 | Is `input` explicitly listed in `server-scopes`? (`control` is not sufficient) |
| attach returns -32003 | Is `attach` explicitly listed in `server-scopes` and requested in `noa.hello`? |
| attach returns -32007 | Another attach is reserved or active for that pane; detach it or wait for an abandoned reservation to expire |
| Raw channel closes with 1008 / -32008 | Request a new attach token and send it as the first binary frame within 10 seconds; do not replay an old token |
| stateChanged never arrives | Does `subscribe`'s `events` include `state_changed`? / busy, attention, and name changes are immediate; cwd and preview changes can lag up to 500ms |
| Can't connect from the LAN | Is `server-bind` still `127.0.0.1` (the default)? (see "LAN exposure procedure" in §1) / is the macOS firewall blocking the connection? (check System Settings → Network → Firewall, or allow incoming connections for the `noa` app) |
| During development: tests fail with PermissionDenied | The TCP tests in `cargo test -p noa-ipc` cannot bind loopback inside the sandbox → run with the sandbox disabled (same as noa-pty) |

## 8. Manual smoke test

```sh
# 1. Add server-enable=true to the config and start noa
lsof -nP -iTCP:61771 -sTCP:LISTEN                      # confirm LISTEN
TOKEN=$(cat ~/.config/noa/server-token)
websocat ws://127.0.0.1:61771/ <<EOF
{"jsonrpc":"2.0","id":1,"method":"noa.hello","params":{"protocolVersion":1,"token":"$TOKEN","scopes":["read"]}}
{"jsonrpc":"2.0","id":2,"method":"noa.listPanels","params":{}}
EOF
```

Set `server-enable = false` again and restart → also confirm that LISTEN disappears from `lsof` (FR-1).
