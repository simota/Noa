# noa-server Client API Specification (protocolVersion 1)

A protocol reference for connecting to noa from external clients (CLI, dashboard, iOS app, etc.).
For operational procedures (enabling, tokens, troubleshooting) see `docs/runbooks/noa-server.md`; for design background see `docs/specs/noa-server.md`.

## 1. Transport

- **WebSocket over TCP**: `ws://<server-bind>:<server-port>/` (default bind `127.0.0.1`, default port `61771`). No TLS. The default is a loopback-only bind — setting `server-bind` (e.g. `0.0.0.0`) opts in to a bind address reachable directly from other hosts on the LAN (v2). Since there's still no TLS, a LAN bind should only be used on a trusted network. On untrusted networks, remote access should still go through a tunnel endpoint such as SSH/Tailscale, as before.
- Messages are **JSON-RPC 2.0** over WS text frames. ≤ 1 MiB per message, ≤ 256 KiB per frame (exceeding either closes the connection).
- Max 32 concurrent connections (accepting beyond that immediately closes the connection).
- **Connection deadlines**: the WS handshake must complete within 5 seconds of connecting (an absolute deadline — even a connection trickling bytes slowly to dodge per-read timeouts cannot exceed this), and `noa.hello` must succeed within 10 seconds of connecting. Connections that exceed these are closed server-side.

## 2. JSON-RPC conventions

- Request: `{"jsonrpc":"2.0","id":<number|string>,"method":"...","params":{...}}`
- Success response: `{"jsonrpc":"2.0","id":<echo>,"result":{...}}` / failure: `{"jsonrpc":"2.0","id":<echo>,"error":{"code":...,"message":"..."}}`
- Server-to-client notifications have no `id`: `{"jsonrpc":"2.0","method":"noa.stateChanged","params":{...}}`
- **Forward compatibility (additive-only)**: unknown fields on known methods are ignored rather than causing an error. Unknown methods return `-32601` but **the connection stays open**. Only a breaking change bumps the `protocolVersion` major. Clients should be implemented to ignore unknown fields and unknown notifications.
- `id` may only be `number | string` per spec. Any other `id` (missing, `null`, object, array, boolean) results in `-32600` `InvalidRequest` on **every** method, including `noa.hello`, and is not dispatched (methods with side effects are not executed either). The connection stays open.

### ID representation

`windowGroupId` / `windowId` / `paneId` / `subscriptionId` are u64 on the server side, but on the wire they are **decimal strings** (e.g. `"42"`), since they can exceed JS's safe integer range (2^53). Both string and integer are accepted on receipt, but string is recommended when sending. ID hierarchy:

```
windowGroup (logical window) ─▶ window (native tab) ─▶ pane
```

`paneId` is stable within a server session and never reused. Using it after the pane has closed returns `-32002`.

## 3. Connection establishment flow

1. WS upgrade. Optionally include an `Authorization: Bearer <token>` header (pre-authenticates if sent).
2. **Send `noa.hello` first** (required). If the header wasn't used, present the token via `params.token`.
3. After hello succeeds, methods can be called within the range of `grantedScopes`.

Any other method before hello returns `-32001`. A major mismatch returns `-32006`.

## 4. Scopes

| Scope | Methods it covers |
|---------|-------------|
| `read` | listPanels / getText / getGrid / subscribe / unsubscribe |
| `control` | focusPane / newTab / split / closePane |
| `input` | sendText |

`grantedScopes` = the intersection of hello's `scopes` (requested) and the server's `server-scopes` config. `control`/`input` are granted only when explicitly allowed server-side. Methods on an ungranted scope return `-32003`.

## 5. Method reference

### noa.hello

| params | type | required | description |
|--------|----|------|------|
| `protocolVersion` | number | ✓ | the client's major version. Currently `1` |
| `token` | string | optional if header-authenticated | Bearer token |
| `scopes` | string[] | — (omitted = `[]`) | requested scopes |

result: `{"protocolVersion":1,"grantedScopes":["read"],"serverVersion":"0.1.2"}`

### noa.listPanels — requires read

params: `{}` / result: `{"panels":[Panel]}` (all panels across all window groups. Quick Terminal panels are excluded, same as in the sidebar)

### noa.getText — requires read

| params | type | required | description |
|--------|----|------|------|
| `paneId` | string | ✓ | |
| `source` | `"screen"` \| `"scrollback"` | ✓ | screen = visible screen only / scrollback = scrollback + visible screen combined |
| `maxBytes` | number | — (default 262144) | UTF-8 byte limit. Server-side **clamped to 1 MiB (1048576 bytes)** (requesting more isn't rejected, just clamped) |

result: `{"paneId":"1","text":"..."}` — when truncated, the **tail is kept in preference** and `"truncated":true` is added (only appears when true).

### noa.getGrid — requires read

| params | type | required | description |
|--------|----|------|------|
| `paneId` | string | ✓ | |
| `startRow` | number | ✓ | absolute row. Row 0 = the oldest scrollback row |
| `rowCount` | number | ✓ | effective limit of 2048 rows per request |

result: `{"paneId":"1","cols":80,"startRow":0,"rows":[Row],"hasMore":false}`

The response is rounded to fit within 256 KiB serialized. When `hasMore:true`, continue with `startRow = previous startRow + rows.length`. If a single row exceeds the limit by itself, `-32005` is returned.

### noa.sendText — requires input

params: `{"paneId":"1","text":"ls\n","paste":true}` / result: `{"ok":true}`

Injects UTF-8 text into the target panel's pty. Include `\n` to send a newline.

- `paste` (optional, default `true`): if the panel is in bracketed paste mode, this is automatically wrapped as a paste — existing behavior.
- `paste:false`: raw injection with no bracketed-paste wrapping. The `text`'s UTF-8 byte sequence is written to the pty as-is. To send a bare Enter, use `{"text":"\r","paste":false}` (useful for emulating keypresses into TUI apps).

### noa.focusPane — requires control

params: `{"paneId":"1"}` / result: `{"ok":true}` — brings the window to the front and focuses the panel.

### noa.newTab — requires control

params: `{"windowId":"..."}` (optional. Either a `windowId` or a window-group id resolves. Omitted defaults to the active window)
result: `{"paneId":"7"}` — the initial panel id of the created tab.

### noa.split — requires control

params: `{"paneId":"1","direction":"horizontal"|"vertical"}` — horizontal = side-by-side, vertical = stacked.
result: `{"paneId":"8"}` — the created pane's id.

### noa.closePane — requires control

params: `{"paneId":"1"}` / result: `{"ok":true}`

The `control` scope is treated as authorized automation, so the pane closes immediately even if a process is running, without going through the GUI confirmation dialog (the confirm dialog normally shown by e.g. cmd+w is skipped). `ok:true` is returned only after the close has actually been dispatched.

### noa.subscribe — requires read

| params | type | required | description |
|--------|----|------|------|
| `events` | (`"state_changed"` \| `"output"`)[] | ✓ | event types to subscribe to |
| `paneIds` | string[] | — | omitted = all panels. When given, filters both `state_changed` and `output` events to this set |

result: `{"subscriptionId":"1"}`

Max 16 per connection (slots freed by `unsubscribe` are immediately available again). A `subscribe` call beyond the limit returns `-32005` ("subscription limit exceeded") without closing the connection.

### noa.unsubscribe — requires read

params: `{"subscriptionId":"1"}` / result: `{"ok":true}`

### Mutation execution semantics

`sendText` / `focusPane` / `newTab` / `split` / `closePane` execute via a round trip to the UI thread, and return `-32603` (internal) if they time out after 2 seconds. **The operation may still execute later, even after the timeout (at-least-once).** Blindly retrying on a failure response can result in double execution.

## 6. Notifications (server → client)

### noa.stateChanged

```json
{"jsonrpc":"2.0","method":"noa.stateChanged","params":{"panels":[Panel]}}
```

Delivers **only the Panels that changed or were added** when panel metadata changes. Changes to busy / attention / name are reflected immediately; changes to cwd / preview may lag up to 500ms. **There is no v1 notification for panel deletion** — if an operation on a known paneId returns `-32002`, resync with `noa.listPanels`. If `subscribe`'s `paneIds` was specified, this array is filtered to Panels within that set (if zero entries in the set changed, no notification is sent at all). May optionally carry `"dropped":true` (same marker as `output`, for subscription queue overflow — see below).

### noa.output

```json
{"jsonrpc":"2.0","method":"noa.output","params":{"paneId":"1","lines":[Row]}}
```

Delivers panel-output updates as **only the visible rows that changed, coalesced at ≥16ms intervals** (with color runs). `Row.row` is the absolute row number. Treat each row as a full replacement, not a patch.

### The dropped marker

When the subscription queue overflows, the oldest notifications are dropped and the next notification of that same type carries `"dropped":true` (only appears when true). On receiving it, it's recommended to refetch the full state of that subscription via `listPanels` / `getGrid`.

## 7. Data types

### Panel

```json
{
  "windowGroupId": "1", "windowId": "140234...", "paneId": "3",
  "name": "zsh", "cwd": "/Users/me/src",
  "branch": "main", "process": "vim",
  "busy": true, "attention": false,
  "preview": [Row]
}
```

`branch` / `process` are **omitted as keys** when unknown. `preview` is the sidebar-equivalent last few rows (with color runs). Each `Row.row` in `preview` is **not an absolute row number** but a 0-based preview-row index (first row is 0) — note this differs in meaning from `noa.getGrid`'s `Row.row` (absolute row).

### Row / Span

```json
{ "row": 120, "spans": [
    { "text": "cargo build", "fg": "#c6d0f5", "attrs": ["bold"] },
    { "text": " done", "fg": 2 }
] }
```

| Span field | Type | Description |
|----------------|----|------|
| `text` | string | text folded from consecutive same-styled cells |
| `fg` / `bg` | `"#rrggbb"` \| number | truecolor is a hex string, palette colors are a 0-255 integer. **The terminal's default color has the key omitted** — render it using the client's own theme default |
| `attrs` | string[] | omitted = none. Values: `bold` `faint` `italic` `underline` `double_underline` `curly_underline` `dotted_underline` `dashed_underline` `blink` `inverse` `invisible` `strikethrough` `overline` |

## 8. Error codes

| code | meaning |
|------|------|
| `-32700` / `-32600` / `-32601` / `-32602` / `-32603` | JSON-RPC standard (parse / invalid request / method not found / invalid params / internal) |
| `-32001` | authentication failure (token mismatch, or a method call before hello) |
| `-32002` | unknown paneId / windowId |
| `-32003` | insufficient scope |
| `-32004` | panel disappeared mid-execution |
| `-32005` | payload exceeded (request/response) |
| `-32006` | protocolVersion major mismatch |

## 9. Full session example

```json
→ {"jsonrpc":"2.0","id":1,"method":"noa.hello","params":{"protocolVersion":1,"token":"<hex64>","scopes":["read","input"]}}
← {"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"grantedScopes":["read","input"],"serverVersion":"0.1.2"}}
→ {"jsonrpc":"2.0","id":2,"method":"noa.listPanels","params":{}}
← {"jsonrpc":"2.0","id":2,"result":{"panels":[{"windowGroupId":"1","windowId":"105553...","paneId":"1","name":"zsh","cwd":"/Users/me","busy":false,"attention":false,"preview":[]}]}}
→ {"jsonrpc":"2.0","id":3,"method":"noa.subscribe","params":{"events":["output"],"paneIds":["1"]}}
← {"jsonrpc":"2.0","id":3,"result":{"subscriptionId":"1"}}
→ {"jsonrpc":"2.0","id":4,"method":"noa.sendText","params":{"paneId":"1","text":"echo hi\n"}}
← {"jsonrpc":"2.0","id":4,"result":{"ok":true}}
← {"jsonrpc":"2.0","method":"noa.output","params":{"paneId":"1","lines":[{"row":42,"spans":[{"text":"hi"}]}]}}
```

## 10. Client implementation checklist

- [ ] Ignore unknown fields and unknown notifications (assumes FR-19)
- [ ] Keep IDs as strings (don't parse u64 into a number — can exceed 2^53)
- [ ] On receiving `-32002`, resync via `listPanels` (there is no deletion notification)
- [ ] Full refetch on receiving `dropped:true`
- [ ] Account for at-least-once semantics when auto-retrying a failed mutation
- [ ] Handle omitted `fg`/`bg` and the conditional presence of `truncated`/`dropped`/`hasMore`
- [ ] Redo hello on reconnect (subscriptions are lost per-connection)
