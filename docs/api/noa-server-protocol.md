# noa-server Client API Specification (protocolVersion 3)

A protocol reference for connecting to noa from external clients (CLI, dashboard, iOS app, etc.).
For operational procedures (enabling, tokens, troubleshooting) see `docs/runbooks/noa-server.md`; for design background see `docs/specs/noa-server.md`.

## 1. Transport

- **WebSocket over TCP**: `ws://<server-bind>:<server-port>/` (default bind `127.0.0.1`, default port `61771`). No TLS. The default is a loopback-only bind: setting `server-bind` (e.g. `0.0.0.0`) opts in to a bind address reachable directly from other hosts on the LAN (v2). Since there's still no TLS, a LAN bind should only be used on a trusted network. On untrusted networks, remote access should still go through a protected tunnel endpoint such as SSH or Tailscale. When using a tunnel, the network between endpoints is assumed to be protected by the tunnel.
- The control endpoint at `/` uses **JSON-RPC 2.0** over WS text frames. Messages are ≤ 1 MiB and individual frames are ≤ 256 KiB. Exceeding either closes the connection with close code **1009** and reason `message too big`; the server reads and discards the rest of the frame for up to 2 seconds first, so the client can read the close frame. `noa_ipc::MAX_SEND_TEXT_BYTES` (250 KiB of JSON-encoded `text`) is the largest `noa.sendText` text that fits one frame.
- The attach endpoint at `/attach` is a separate raw binary channel described in [§6](#6-raw-attach-channel). It never carries JSON-RPC or base64-wrapped payloads.
- Max 32 concurrent connections, counting both control and raw attach WebSockets. A connection beyond that completes the WebSocket handshake and is closed with close code **1013** (Try Again Later) and reason `too many connections`; retrying later can succeed. At most 4 such refusals run at once; past that, and for the per-address cap of a LAN bind, the connection is dropped unanswered. Older servers dropped every refused connection unanswered.
- **Connection deadlines**: the WS handshake must complete within 5 seconds of connecting (an absolute deadline; even a connection trickling bytes slowly to dodge per-read timeouts cannot exceed this), and `noa.hello` must succeed within 10 seconds of connecting. Connections that exceed these are closed server-side.

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
A `noa.hello` with a wrong or missing token returns `-32001` and the server then closes the
connection; retry on a new connection.

An upgrade request carrying an `Origin` header (every browser sends one) is closed with policy code
1008 right after the upgrade unless the origin's host is `localhost`, a loopback IP, or the same IP literal as the request's
`Host`. Native clients send no `Origin` and are unaffected.

## 4. Scopes

| Scope | Methods it covers |
|---------|-------------|
| `read` | listPanels / getText / getGrid / subscribe / unsubscribe |
| `control` | focusPane / newTab / split / closePane |
| `input` | sendText; also newTab / split when their `config` runs or types something |
| `attach` | attach / detach / resizePane |

`grantedScopes` = the intersection of hello's `scopes` (requested) and the server's `server-scopes` config. `control`/`input`/`attach` are granted only when explicitly allowed server-side. The default `server-scopes=read` never grants attach access. Methods on an ungranted scope return `-32003`.

## 5. Method reference

### noa.hello

| params | type | required | description |
|--------|----|------|------|
| `protocolVersion` | number | ✓ | the client's major version. Currently `3` |
| `token` | string | optional if header-authenticated | Bearer token |
| `scopes` | string[] | — (omitted = `[]`) | requested scopes |

result: `{"protocolVersion":3,"grantedScopes":["read"],"serverVersion":"0.1.2"}`

### noa.listPanels (requires read)

params: `{}` / result: `{"panels":[Panel]}` (all panels across all window groups. Quick Terminal panels are excluded, same as in the sidebar. A panel with `attachable:false` is discoverable/readable but has no raw `noa.attach` endpoint.)

### noa.getText (requires read)

| params | type | required | description |
|--------|----|------|------|
| `paneId` | string | ✓ | |
| `source` | `"screen"` \| `"scrollback"` | ✓ | screen = live screen only (regardless of the pane's local scrollback position) / scrollback = scrollback + live screen combined |
| `maxBytes` | number | — (default 262144) | UTF-8 byte limit. Server-side **clamped to 1 MiB (1048576 bytes)** (requesting more isn't rejected, just clamped) |

result: `{"paneId":"1","text":"..."}`; when truncated, the **tail is kept in preference** and `"truncated":true` is added (only appears when true). `"alternateScreen":true` (only appears when true) means the pane's active screen was the alternate screen at the time of the read: it keeps no scrollback, so a `scrollback` read returns only what the screen shows.

### noa.getGrid (requires read)

| params | type | required | description |
|--------|----|------|------|
| `paneId` | string | ✓ | |
| `startRow` | number | ✓ | session-absolute row within the current `coordinateGeneration` |
| `rowCount` | number | ✓ | effective limit of 2048 rows per request |

result: `{"paneId":"1","cols":80,"startRow":0,"coordinateGeneration":4,"oldestRow":120,"nextRow":220,"rows":[Row],"hasMore":false}`

`coordinateGeneration` is the opaque identity of the row-coordinate space. Clients must discard every cached row and restart from the tail when it changes. It advances after operations that rebuild or collapse row coordinates, including scrollback clear, column reflow, full reset, and primary/alternate screen switches. Ordinary front eviction keeps the generation stable and preserves the coordinates of surviving rows.

`oldestRow` is the first retained coordinate and `nextRow` is the exclusive end of the retained range. To load the current tail, request `startRow = max(oldestRow, nextRow - desiredRowCount)` after an initial bounds probe. Coordinates below `oldestRow` have been evicted and remain invalid within that generation.

The response is rounded to fit within 256 KiB serialized. `hasMore:true` means only that the requested range was truncated by the 2048-row or 256 KiB response cap; it does not mean that newer or older history exists. Continue the same range from one past the last returned `Row.row`. If a single row exceeds the limit by itself, `-32005` is returned.

### noa.sendText (requires input)

params: `{"paneId":"1","text":"ls\n","paste":true}` / result: `{"ok":true}`

Injects UTF-8 text into the target panel's pty. Include `\n` to send a newline.

- `paste` (optional, default `true`): if the panel is in bracketed paste mode, this is automatically wrapped as a paste (existing behavior).
- `requirePasteMode` (optional, default `false`): with `paste` true, a panel that has not enabled bracketed paste (DECSET 2004) gets nothing written, and the request fails with **`-32010`** (`pane N has not enabled bracketed paste yet (its program may still be starting); nothing was sent`). The check and the write happen in one step on the UI thread. An empty `text` with this flag writes nothing either way, so it probes whether a just-started program is ready for a multi-line paste. Ignored with `paste:false`, and by servers older than the flag (which then send as usual).
- `refuseShell` (optional, default `false`): the server probes the panel's foreground process at the moment of the request, and when it is a plain shell (`sh`, `bash`, `zsh`, `fish`, `dash`, `ksh`, `tcsh`, `csh`, `nu`, `pwsh`) nothing is written and the request fails with **`-32010`** (`pane N is at a zsh prompt, where the text would run as commands; nothing was sent`). For text meant for an agent: the panel's `process` field is polled every 1–4 s and can still name an agent that has exited. A panel whose process cannot be probed (a remote panel) is not refused. Applies with either `paste` value; ignored by servers older than the flag (which then send as usual).
- `paste:false`: raw injection with no bracketed-paste wrapping. The `text`'s UTF-8 byte sequence is written to the pty as-is. To send a bare Enter, use `{"text":"\r","paste":false}` (useful for emulating keypresses into TUI apps).
- Sending non-empty text that the panel accepts acknowledges its notification, as focusing it does (refused input, such as to an exited panel, leaves it unread): its `attention` (and unread bell) drops, and a subscriber receives that `stateChanged` before the text reaches the pty. A client answering an agent can then wait for its next `attention`.

### noa.focusPane (requires control)

params: `{"paneId":"1"}` / result: `{"ok":true}`; brings the window to the front and focuses the panel.

### noa.newTab (requires control)

params: `{"windowId":"..."}` (optional. Either a `windowId` or a window-group id resolves. Omitted defaults to the active window)
result: `{"paneId":"7"}`; the initial panel id of the created tab.

Also accepts the [spawn extensions](#spawn-extensions-newtab--split).

### noa.split (requires control)

params: `{"paneId":"1","direction":"horizontal"|"vertical"|"auto"|"float"}`; horizontal = side-by-side, vertical = stacked.
result: `{"paneId":"8"}`; the created pane's id.

`auto` lets the server choose: with no pane yet spawned with `parentPaneId` equal to `paneId`, it splits `paneId` side-by-side; otherwise it splits the largest such pane along its longer side. When no candidate has room left, the new pane opens in a new tab of the same window instead. Also accepts the spawn extensions below.

`float` opens a floating pane over `paneId` instead of splitting: collapsed to its title strip with `focus:false`, open and focused otherwise. It needs `parentPaneId` (invalid params without it), since `noa.listPanels` lists a float only as an orchestrator's worker; other floats stay out of every listing. A pane holds at most 8 floats; past that the request fails with an internal error. A server older than `float` rejects the value as invalid params.

### Spawn extensions (newTab / split)

Optional fields for an orchestrator that starts worker panes (see [Agent workflows](../AGENT_WORKFLOW.md#orchestrating-worker-panes)). A request with none of them, and a fixed `direction`, behaves exactly as before.

| field | type | description |
|---|---|---|
| `config.command` | string[] | argv to run instead of the shell, spawned directly (no shell). `argv[0]` is looked up on `config.env.PATH` when given |
| `config.cwd` | string | working directory; ignored unless it is an existing directory |
| `config.env` | object | extra environment, applied after Noa's own |
| `config.initialInput` | string | written raw to the new pane right after spawn |
| `config.waitAfterCommand` | boolean | keep the pane open after its child exits, so its output and `Panel.exit` stay readable until it is closed |
| `label` | string | free text shown as `Panel.label` (at most 128 bytes) |
| `parentPaneId` | string | the orchestrator's pane; shown as `Panel.parentPaneId` and used by `direction:"auto"` |
| `focus` | boolean | `false` puts keyboard focus back where it was after the spawn. Omitted means `true`. A spawn that fails (no room to split) always puts focus back |
| `maxChildren` | number | with `parentPaneId`: refuse the spawn with **`-32011`** when that pane already has this many children (`pane P already has N children (L live, E exited); maxChildren is M`). It counts every pane in every window whose parent is `parentPaneId`, exited panes held open included, on the UI thread before the pane is created, so concurrent spawns cannot pass it. Ignored without `parentPaneId`, and by servers older than the field |
| `autoApprove` | boolean | `noa.newTab` only: turn the new tab's agent-CLI auto approval (the per-tab **Toggle Auto Approve**) on or off instead of the `auto-approve` config default. Panes split into the tab later share it. `true` needs the `input` scope; on `noa.split` the field is `-32602`. Servers older than the field ignore it |

A `config` with `command`, `initialInput`, or `env` also needs the `input` scope (`-32003` otherwise). An empty `command`, an `env` key that is empty or contains `=`, or an over-long `label` is `-32602`, and so is a `label` starting with `board:` together with `parentPaneId` (that label names an orchestrator's board, which has no parent). An unknown `parentPaneId` is `-32002`.

### noa.closePane (requires control)

params: `{"paneId":"1"}` / result: `{"ok":true}`

The `control` scope is treated as authorized automation, so the pane closes immediately even if a process is running, without going through the GUI confirmation dialog (the confirm dialog normally shown by e.g. cmd+w is skipped). `ok:true` is returned only after the close has actually been dispatched.

### noa.attach (requires attach)

params: `{"paneId":"1"}`

result:

```json
{"attachToken":"<opaque-one-time-token>","attachUrl":"ws://127.0.0.1:61771/attach"}
```

Reserves a single raw attach lease for the pane and returns the fixed attach endpoint plus a short-lived, one-time correlation token. Only one reserved or active attach is allowed per pane; another request returns `-32007` without disturbing the existing lease. The token is not embedded in the URL and must not be logged.

Open `attachUrl` as a second WebSocket and send `attachToken` as the first **binary** frame within 10 seconds. A successful server sends the synthetic VT seed in one or more binary messages of at most 64 KiB, followed by an empty binary message that marks the seed boundary; later binary messages are live PTY bytes. A mismatched, expired, replayed, or non-binary token frame closes the WebSocket with policy code `1008` and the stable, non-secret reason `-32008 attach handshake failure`.

### noa.detach (requires attach)

params: `{"paneId":"1"}` / result: `{"ok":true}`

Releases the pane's attach lease and closes its raw subscription. It does not close the pane or terminate the remote process.

Closing the control connection does the same for every lease it holds: an unredeemed attach token stops working and an active raw channel is closed. Keep the control connection open for as long as the raw channel is in use.

### noa.resizePane (requires attach)

params: `{"paneId":"1","cols":120,"rows":40}` / result: `{"ok":true}`

`cols` and `rows` must each be in `1..=4096`, and their product must not
exceed 1,048,576 cells. Larger grids return `-32602` before reaching the pane
backend.

Resizes the server-side terminal grid first and then updates the PTY winsize. A concurrent local GUI resize is last-writer-wins.

### noa.subscribe (requires read)

| params | type | required | description |
|--------|----|------|------|
| `events` | (`"state_changed"` \| `"output"`)[] | ✓ | event types to subscribe to |
| `paneIds` | string[] | — | omitted = all panels. When given, filters both `state_changed` and `output` events to this set |

result: `{"subscriptionId":"1"}`

`paneIds`, when present, may list at most 1024 ids; more returns `-32602`. Max 16 per connection (slots freed by `unsubscribe` are immediately available again). A `subscribe` call beyond the limit returns `-32005` ("subscription limit exceeded") without closing the connection.

### noa.unsubscribe (requires read)

params: `{"subscriptionId":"1"}` / result: `{"ok":true}`

### Repository widgets (capability `repoWidgetsV1`)

The capability is advertised only when the repository widget service is
available. All widget params are objects, use camelCase, and reject unknown
fields. `repo` is an absolute path to the intended worktree. Widget revisions
and event sequences are unsigned JSON integers; `ownerPaneId` uses the normal
wire ID string representation.

| Method | Scope | Params |
| --- | --- | --- |
| `noa.widget.list` | read | `repo` |
| `noa.widget.describe` | read | `repo`, `id` |
| `noa.widget.open` | control | `repo`, exactly one of `id` or `definition`, `requestId`; optional `data`, `ownerPaneId` |
| `noa.widget.get` | read | `instanceId` |
| `noa.widget.set` | control | `instanceId`, `data`, `expectedRevision`, `requestId` |
| `noa.widget.events` | control | `instanceId`, `consumerId`; optional `afterSeq` (0), `limit` (32, at most 64) |
| `noa.widget.ack` | control | `instanceId`, `consumerId`, `throughSeq`, `requestId` |
| `noa.widget.report` | control | `instanceId`, `consumerId`, `eventId`, `expectedVersion`, `state`, `requestId`; optional `message` (requires `repoWidgetOperationsV1`) |
| `noa.widget.close` | control | `instanceId`, `requestId` |
| `noa.widget.forget` | control | `instanceId` |

These calls run on the widget service's single writer, outside the UI thread.
Mutations have a four-second service wait; an uncertain result must be retried
with the identical request ID and params. Request IDs deduplicate successful
mutations; reusing one for different params is a conflict. `set` replaces the
complete data after schema validation and a revision comparison. `events`
claims one consumer and retains events until cumulative acknowledgment.
`forget` is idempotent by instance ID and removes only stopped instances with
no pending events, including their retry history.

`list` returns valid `definitions` alongside per-package `diagnostics`
(`path`, `message`). A malformed package does not hide valid siblings; an
unsafe or unreadable registry root still fails the call.

#### Processing reports (capability `repoWidgetOperationsV1`)

This additional capability is advertised when the service accepts `report`.
It separates durable event receipt from the consumer's reported processing
outcome. Neither `ack` nor `set` marks an operation complete.

A queued action starts at `state: "accepted"`, `version: 1`. After receiving
it through `events`, its registered consumer can report `running`, `waiting`,
`completed` or `failed`. `expectedVersion` must match the operation's current
version; every accepted report increments it. Terminal `completed` and
`failed` records cannot change. Identical retries with the original
`requestId` and params return the original result before checking version or
terminal state. A different consumer, an undelivered event or a stale version
is rejected. `message` defaults to an empty string and is limited to 1,024
UTF-8 bytes; control characters other than newline and tab are rejected.

```json
{"jsonrpc":"2.0","id":17,"method":"noa.widget.report","params":{"instanceId":"wi_0123456789abcdef0123456789abcdef","consumerId":"test-run-42","eventId":"we_0123456789abcdef0123456789abcdef_1","expectedVersion":1,"state":"running","message":"Running the failed tests","requestId":"test-run-42-started"}}
```

The result contains `instanceId`, `eventId`, `version` and `state`. `get`
returns all retained `operations` in ascending `seq` order, with `eventId`,
`seq`, `actionId`, optional original UI `actionRequestId`, `version`, `state`,
`message`, `acceptedAtMs` and `updatedAtMs`. The instance also exposes
`dataUpdatedAtMs`. Timestamps use Unix milliseconds; old stored events may
have unknown timestamps and no original request ID. Already-acknowledged
events from older stores are not reconstructed.

Reports remain durable after acknowledgment and restart. The registered
consumer can report a final outcome for a stopped instance without reopening
its window. Processing history shares the existing bounded instance storage
and retry ledgers. Explicit `forget` deletes that history, including any
operation without a final outcome; it never implies completion. Existing
producers may continue without reports, and their actions remain unconfirmed.

The native host uses a separate inherited pipe tied to one instance; it never
authenticates with the public control token. The public methods keep the
existing trusted-controller scope model, not per-repository authorization.
The manifest contract, response fields, lifecycle, durable event processing,
limits and CLI examples are in [Repository widgets](../REPOSITORY_WIDGETS.md).

### Mutation execution semantics

`sendText` / `focusPane` / `newTab` / `split` / `closePane` / `resizePane` execute via a round trip to the UI thread, and return `-32009` (outcome unknown) if they time out after 2 seconds. **The operation may still execute later, even after the timeout (at-least-once).** Blindly retrying on a failure response can result in double execution.

## 6. Raw attach channel

Protocol version 3 requires cell-exact seed replay. `CSI > Ph ; Pl ; Pw $ v`
prints a new cell with scalar `(Ph << 16) | Pl` and stored width `Pw` (1 or 2),
without joining the preceding cell. `CSI > Ph ; Pl $ x` appends a scalar to
that cell without changing its width or cursor. These seed-only commands
preserve cells created under different mode-2027 settings. Both client and
server must use version 3; older versions are rejected during `noa.hello`.

After the first-frame token handshake, every data message on `/attach` is binary and carries at most 64 KiB of application payload:

- Server → client: a synthetic repaint VT seed split across as many messages as needed, one empty binary seed terminator, then lossless PTY output in order. The terminator is a boundary marker and contributes no VT bytes.
- Client → server: raw terminal input bytes, split across as many messages as needed while preserving byte order. Arrow keys, control sequences, mouse reports, paste bytes, and escape timing are not converted to text or JSON.

The seed reconstructs the visible grid and terminal modes; it intentionally excludes scrollback and the window title. Fetch scrollback independently with `noa.getText` (`source=scrollback`) or `noa.getGrid`, and use `Panel.name` for the title. The server buffers output behind the seed boundary and uses a dedicated byte-counted 1 MiB queue. If output remains backpressured for 2 seconds, the raw channel disconnects rather than dropping bytes. A client should re-run `noa.attach` and apply the new seed; it must not replay input queued for a previous attach generation.

Client-to-PTY input is also bounded end to end: at most 8 MiB per pane may be
pending across the attach, IO, and PTY-writer queues, with small messages
charged at least 1 KiB to bound container overhead. Exceeding that budget
disconnects the raw channel instead of retaining unbounded input.

Closing a raw WebSocket tears down only the attach subscription. Attach connections share the server's global 32-connection limit with control connections.

## 7. Notifications (server → client)

### noa.stateChanged

```json
{"jsonrpc":"2.0","method":"noa.stateChanged","params":{"panels":[Panel]}}
```

Delivers **only the Panels that changed or were added** when panel metadata changes. Changes to busy / attention / name are reflected immediately; changes to cwd / preview may lag up to 500ms. **There is no notification for panel deletion**: if an operation on a known paneId returns `-32002`, resync with `noa.listPanels`. If `subscribe`'s `paneIds` was specified, this array is filtered to Panels within that set (if zero entries in the set changed, no notification is sent at all). May optionally carry `"dropped":true` (same marker as `output`, for subscription queue overflow; see below).

### noa.output

```json
{"jsonrpc":"2.0","method":"noa.output","params":{"paneId":"1","coordinateGeneration":4,"lines":[Row]}}
```

Delivers panel-output updates as **only the visible rows that changed, coalesced at ≥16ms intervals** (with color runs). `coordinateGeneration` and `Row.row` use the same coordinate space as `noa.getGrid`. Ignore the lines and refetch the tail if the generation differs from the cached grid. Treat each row as a full replacement, not a patch.

### The dropped marker

When the subscription queue overflows, the oldest notifications are dropped and the next notification of that same type carries `"dropped":true` (only appears when true). On receiving it, it's recommended to refetch the full state of that subscription via `listPanels` / `getGrid`.

## 8. Data types

### Panel

```json
{
  "windowGroupId": "1", "windowId": "140234...", "paneId": "3",
  "name": "zsh", "cwd": "/Users/me/src",
  "branch": "main", "process": "vim",
  "busy": true, "attention": false,
  "attachable": true,
  "preview": [Row],
  "surfaceId": "0x2f9c0e5a7b1d4c33",
  "agent": { "kind": "claude", "state": "input", "detail": "", "seq": 7, "blocked": "question" },
  "exit": { "code": 0 },
  "parentPaneId": "1", "label": "fix-tests",
  "spawn": { "argv": ["claude", "--model", "sonnet"], "startedAt": 1763700000 }
}
```

The last six keys are optional and omitted when they do not apply:

- `surfaceId`: the value of `NOA_SURFACE_ID` in the pane's child environment, so a process inside a pane can find its own panel.
- `agent`: present when the foreground process is a recognized coding agent (`kind` is `claude`, `codex`, or `agy`) or when an `OSC 777;noa-agent` report arrived. `state`/`detail` mirror the last report (`running`, `permission`, `input`, `finished`, `error`; absent after `clear`). `seq` counts reports for this pane, every report including `clear` adding one, so a client that sends input can ignore a report that predates it. `blocked` is `"permission"` or `"question"`, present only while the last report was an `OSC 7501` blocked report: `kind=permission` gives state `permission` with `blocked:"permission"`; any other kind (or none) gives state `input` with `blocked:"question"`, a dialog where Enter picks the first choice; `kind=auth` gives state `error` and no `blocked`. Any later report clears it, so a `state:"input"` without `blocked` is an agent idle at its prompt (or a hook report). When the foreground process is not known yet, `kind` comes from the report's `app=` or from `spawn.argv[0]`.
- `exit`: present only while a pane whose child exited is held open (`config.waitAfterCommand`, or an abnormal exit). `code` is omitted when the platform reported none.
- `parentPaneId` / `label`: as given to the spawn extensions.
- `spawn`: for a pane a spawn extension started with `config.command`: the `argv` it ran and `startedAt`, Unix seconds when the pane was created. Absent for a pane started with a shell or any other way, and not kept across a relaunch.

A pane is listed even before its first output (a pane spawned with `parentPaneId` or `label` from the moment the spawn returns); until then its `name` and `cwd` are empty and it has no `agent`.

Changes to `agent.seq`, `process`, and `exit` are pushed in `noa.stateChanged` without waiting for the coarse refresh tick.

`branch` / `process` are **omitted as keys** when unknown. `attachable` reports whether this exact panel is registered with the raw attach endpoint; clients must not call `noa.attach` for a panel where it is `false`. It defaults to `true` when the key is absent; the field was added while the protocol was still at version 1, and peers from before that fail the `noa.hello` major-version check, so a protocolVersion 3 peer always sends it. `preview` is the sidebar-equivalent last few rows (with color runs). Each `Row.row` in `preview` is **not an absolute row number** but a 0-based preview-row index (first row is 0); note this differs in meaning from `noa.getGrid`'s `Row.row` (absolute row).

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
| `fg` / `bg` | `"#rrggbb"` \| number | truecolor is a hex string, palette colors are a 0-255 integer. **The terminal's default color has the key omitted**: render it using the client's own theme default |
| `attrs` | string[] | omitted = none. Values: `bold` `faint` `italic` `underline` `double_underline` `curly_underline` `dotted_underline` `dashed_underline` `blink` `inverse` `invisible` `strikethrough` `overline` |

## 9. Error codes

| code | meaning |
|------|------|
| `-32700` / `-32600` / `-32601` / `-32602` / `-32603` | JSON-RPC standard (parse / invalid request / method not found / invalid params / internal) |
| `-32001` | authentication failure (token mismatch, or a method call before hello) |
| `-32002` | unknown paneId / windowId |
| `-32003` | insufficient scope |
| `-32004` | panel disappeared mid-execution |
| `-32005` | payload exceeded (request/response) |
| `-32006` | protocolVersion major mismatch |
| `-32007` | attach conflict (the pane already has a reserved or active attach) |
| `-32008` | attach handshake failure |
| `-32009` | outcome unknown: a UI request got no result within 2 seconds, a widget service request within 4 seconds, or a widget state was published but directory sync failed; the operation may have taken effect |
| `-32010` | not ready: `noa.sendText` with `requirePasteMode` to a pane that has not enabled bracketed paste, or with `refuseShell` to a pane whose foreground process is a plain shell; nothing was written |
| `-32011` | limit reached: a spawn over its `maxChildren` |
| `-32019` | widget conflict: stale revision, changed request payload, another consumer, or incompatible lifecycle |
| `-32020` | widget instance or owner pane not found |

WebSocket close codes: `1008` (an `Origin` that is not allowed, or an attach handshake failure), `1009` (a frame or message over the size limit), `1013` (the connection limit).

## 10. Full session example

```json
→ {"jsonrpc":"2.0","id":1,"method":"noa.hello","params":{"protocolVersion":3,"token":"<hex64>","scopes":["read","input"]}}
← {"jsonrpc":"2.0","id":1,"result":{"protocolVersion":3,"grantedScopes":["read","input"],"serverVersion":"0.1.2"}}
→ {"jsonrpc":"2.0","id":2,"method":"noa.listPanels","params":{}}
← {"jsonrpc":"2.0","id":2,"result":{"panels":[{"windowGroupId":"1","windowId":"105553...","paneId":"1","name":"zsh","cwd":"/Users/me","busy":false,"attention":false,"preview":[]}]}}
→ {"jsonrpc":"2.0","id":3,"method":"noa.subscribe","params":{"events":["output"],"paneIds":["1"]}}
← {"jsonrpc":"2.0","id":3,"result":{"subscriptionId":"1"}}
→ {"jsonrpc":"2.0","id":4,"method":"noa.sendText","params":{"paneId":"1","text":"echo hi\n"}}
← {"jsonrpc":"2.0","id":4,"result":{"ok":true}}
← {"jsonrpc":"2.0","method":"noa.output","params":{"paneId":"1","coordinateGeneration":0,"lines":[{"row":42,"spans":[{"text":"hi"}]}]}}
```

## 11. Client implementation checklist

- [ ] Ignore unknown fields and unknown notifications (the additive-only rule, FR-19 in `docs/specs/noa-server.md`)
- [ ] Keep IDs as strings (don't parse u64 into a number; can exceed 2^53)
- [ ] On receiving `-32002`, resync via `listPanels` (there is no deletion notification)
- [ ] Full refetch on receiving `dropped:true`
- [ ] Account for at-least-once semantics when auto-retrying a failed mutation
- [ ] Handle omitted `fg`/`bg` and the conditional presence of `truncated`/`dropped`/`hasMore`
- [ ] Redo hello on reconnect (subscriptions are lost per-connection)
- [ ] Keep the attach token out of URLs, logs, error text, and persisted session state
- [ ] Treat `/attach` as binary-only; re-attach and re-seed after any disconnect
- [ ] Do not buffer or replay user input across attach generations
- [ ] Drain locally generated terminal replies without forwarding them to the remote attach channel
