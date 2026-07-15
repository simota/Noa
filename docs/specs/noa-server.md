# noa-server — locked specification

- slug: `noa-server`
- status: `locked` (signed off 2026-07-11)
- owner: simota
- build-path decision: **apex** (/nexus apex — single run: design → implementation → AC verification → ship. fallback: orbit / feature)
- quality gate: PASS (Judge+Attest 2026-07-11, blocker 0)

## L0 — Vision

**Problem:** noa can only manage panel (window → tab → pane) state inside its own GUI; control from external programs is limited to macOS-only AppleScript. We need a server capability that lets "connected clients" — CLI tools, external apps, AI agents, etc. — connect to a running noa instance, browse the panel list and each panel's state (title, process, busy, output content, etc.), and execute actions or inject text input into a specific panel. (Confirmed with the user at the FRAME checkpoint, 2026-07-11)

**Audience (who):**
- External apps / dashboards (remote monitoring and control of the session list)
- iOS apps (→ network access is mandatory)
- noa's own future features (internal foundation for remote windows, session sharing, etc.)

**Definition of success:** beyond panel listing, state retrieval, actions, and input, clients can also perform **real-time push (subscription) of state changes and output**.

## Reusable assets & constraints (Lens scan, 2026-07-11)

**Reusable:**
- State model: `SessionStore`/`SessionCard` (name/cwd/branch/process/busy/attention/preview), `AppStateSnapshot` (main-thread read projection for AppleScript, `macos_applescript.rs:31`)
- Action injection: dispatch `UserEvent` (WriteText/SpawnTab/ClosePane/AppCommand, etc.) via `EventLoopProxy` — same path as AppleScript
- Action vocabulary: `command_from_applescript_action`
- Input injection: `write_pane_pty_bytes` / bounded `PtyInputQueue`
- Text extraction: `Screen::scrollback_text()` / `selected_text()` / preview_spans / `FrameSnapshot`
- Lifecycle scaffold: `Registration::install(proxy, snapshot)`
- Adding a config key is a standard 5-location change (`noa-config/src/lib.rs` + `parser/overrides.rs`)

**Constraints:**
- The `App` itself is main-thread-exclusive. The server must run on a separate thread and strictly use two paths: mutation via `EventLoopProxy`, reads via a shared `Arc<Mutex<Snapshot>>`
- `Terminal` locks must be held briefly (they contend with the pty feed)
- Networking dependencies such as serde/tokio are not yet introduced (this is a fully new increment)
- IDs are pointer-derived u64 → must be handled as 64-bit/string in the API
- No prior art for authentication/exposure-scope security in this codebase. loopback socket bind may not be possible under sandbox

## SHAPE — Proposal (Spark 2026-07-11, before user confirmation)

**Solution:** a new crate `noa-ipc` — a JSON-RPC over WebSocket server. Sync tungstenite + thread-per-connection + crossbeam (no async runtime). Reads = main-thread-exposed `Arc<Mutex<Snapshot>>`; mutation = `EventLoopProxy<UserEvent>` (same path as AppleScript); output = structured line diffs reusing the `feed.rs` tap + sidebar-extraction logic. Auth = WS over loopback TCP with a mandatory token, with mutating operations in a separate scope. `protocolVersion` is exchanged during handshake (additive-only).

```
 ┌─────────── main thread (App) ───────────────────┐
 │  Terminal ─about_to_wait─▶ Arc<Mutex<Snapshot>> │─read──┐
 │       ▲                                         │       ▼
 │       │ feed.rs tap ─▶ output diff broadcast ───┼─push─▶ ┌──────────────┐
 │  EventLoopProxy<UserEvent> ◀─mutate─────────────┼───────│ noa-ipc      │◀═WS═▶ client
 └─────────────────────────────────────────────────┘       │ (tungstenite │      (CLI/iOS/
                                              token auth    │  per-conn)   │       dashboard)
                                                            └──────────────┘
```

**In-scope (v1):** RPCs `list_panels` / `get_text` / `get_grid` (viewport-limited, paginated, with color attributes) / `send_text` / `focus_pane` / `new_tab` / `split` / `close_pane`; `subscribe` → `state_changed` + `output` (structured line diffs); config `server-enable`/`server-port`/`server-token`; 5 NFRs (CHALLENGE section).

**Out-of-scope (v1):** direct LAN bind / in-process TLS / full keybind vocabulary / per-client subscription policy / raw VT streaming / CRDT/offline / multi-noa remoting. *(Amendment 2026-07-14: LAN bind has since shipped as opt-in `server-bind`; raw VT streaming / PTY attach is superseded for the noa↔noa consumer by `docs/specs/noa-client-mode.md` — see amendment notes in the Out-of-scope and Considered-but-rejected sections below.)*

**Assumptions:** iOS reaches the server via a tunnel / loopback bind is possible even under sandbox (degrade if not) / IDs have a stable u64 representation / `about_to_wait` update frequency satisfies push-latency requirements.

## L1 — Requirements

### Functional (FR)

- **FR-1 (lifecycle/gate):** The server starts only when config `server-enable` (bool, default `false`) is true; if false, it opens no port at all.
- **FR-2 (bind):** The server binds a WebSocket only on `127.0.0.1:<server-port>` (default `61771`) and does not listen on any non-loopback interface. On bind failure, the app continues running and only logs a warning.
- **FR-3 (token provisioning):** On first launch, a token is auto-generated and saved to a file with 0600 permissions, then reused thereafter. If config `server-token` is set, it takes priority and file generation is skipped.
- **FR-4 (handshake):** After connection establishment, the server and client exchange `protocolVersion` (integer major). A connection with a major mismatch is rejected with an error.
- **FR-5 (auth):** The client must present the token via an `Authorization: Bearer <token>` header or in the initial `noa.hello` message; a connection is refused if the token doesn't match.
- **FR-6 (scopes):** Authorization uses 3 scopes: `read` / `control` / `input`. Only scopes listed in config `server-scopes` (comma-separated list, default `read`) may be granted, and the client is granted the intersection of that set with the scopes it requests in `noa.hello`. Calls to methods requiring an ungranted scope are rejected. `control` (focus/tab/split/close) and `input` (send_text) can each be granted only when explicitly listed in `server-scopes`.
- **FR-7 (list_panels):** `noa.listPanels` returns the list of panels across all window groups, each with an ID and metadata (name/cwd/branch/process/busy/attention/preview). Requires `read`.
- **FR-8 (get_text):** `noa.getText` returns the text of a specified panel. `source=screen` returns only the visible screen; `source=scrollback` returns the **entire content, including scrollback and the visible screen** (equivalent to `scrollback_text()`). The response is bounded by `maxBytes` (default 256KB); if exceeded, it is **truncated preferring the tail** and returns `truncated:true` (consistent with NFR-4). Requires `read`.
- **FR-9 (get_grid):** `noa.getGrid` returns a panel's grid paginated by row range (`startRow`/`rowCount`), with each cell expressed as text plus color runs (PreviewSpan format). A single response stays within a bounded size. Requires `read`.
- **FR-10 (focus_pane):** `noa.focusPane` brings the specified panel to the front and focuses it. Requires `control`.
- **FR-11 (new_tab):** `noa.newTab` creates a new tab in the specified window (active window if omitted) and returns the created panel ID. Requires `control`.
- **FR-12 (split):** `noa.split` splits the specified panel in the given direction (`horizontal`/`vertical`) and returns the created pane ID. Requires `control`.
- **FR-13 (close_pane):** `noa.closePane` closes the specified panel. Requires `control`.
- **FR-14 (send_text):** `noa.sendText` injects UTF-8 text into the pty of the specified panel. Requires `input`.
- **FR-15 (subscribe):** `noa.subscribe` / `noa.unsubscribe` start/stop a push subscription for specified event types (`state_changed` / `output`) and panel filters. Requires `read`.
- **FR-16 (state_changed):** When panel metadata changes, a `noa.stateChanged` notification (the same structured snapshot used for sidebar extraction) is sent to subscribed clients.
- **FR-17 (output):** When panel output updates, a `noa.output` notification with color-run-annotated line diffs is sent to subscribed clients. If any drops occur, the notification includes a `dropped` marker.
- **FR-18 (errors):** All methods return failures as JSON-RPC 2.0 error objects, with distinct codes assigned for auth failure, unknown panel, insufficient scope, panel disappearance, payload overflow, and version mismatch.
- **FR-19 (versioning):** The protocol extends in an additive-only manner; major bumps occur only for breaking changes. Definition of "harmless": an unknown method returns the standard error `-32601` and the **connection is kept alive**. Unknown fields within a known method are ignored rather than erroring.

### Non-Functional (CFR, promoted from CHALLENGE)

- **NFR-1 (security):** loopback-only bind by default + authorization required for all methods. Mutating operations (`control`/`input`) are opt-in scopes separate from `read`.
- **NFR-2 (non-blocking):** the terminal and io_thread must never wait on a client. Push uses a bounded `try_send` + drop-oldest + a missed-data marker, so a slow/stalled client never delays rendering or pty feed.
- **NFR-3 (concurrency model):** no async runtime. Only sync tungstenite + thread-per-connection + crossbeam (same concurrency model as io_thread).
- **NFR-4 (bounded serialization):** serialization is bounded. Limited to viewport/row ranges, pagination is mandatory, dirty coalescing ≥16ms. Bulk full-scrollback dumps are prohibited.
- **NFR-5 (versioned protocol):** the protocol is versioned. `protocolVersion` is exchanged at handshake, and a major mismatch rejects the connection.

## L2 — Detail

### Transport & Handshake

- WebSocket over TCP, `ws://127.0.0.1:61771/` (configurable via `server-port`). No TLS (loopback assumed; iOS terminates via tunnel).
- Auth: an `Authorization: Bearer <token>` header on the WS upgrade, or for clients that can't set headers, an `noa.hello` request (`params.token`) sent immediately after connecting. Either is compared against the FR-3 token in constant time.
- Handshake: the client sends `noa.hello { protocolVersion, token, scopes }`, and the server responds with `{ protocolVersion, grantedScopes, serverVersion }`. Current `protocolVersion` is `2`. `grantedScopes` = requested scopes ∩ `server-scopes` (config, default `read` only).
- Any method called before auth and version are established is rejected with `-32001` (auth) / `-32006` (version).

### JSON-RPC 2.0 method table

| Method | Required scope | params | result (summary) |
|----------|-----------|--------|----------------|
| `noa.hello` | — | `{ protocolVersion, token, scopes:[…] }` | `{ protocolVersion, grantedScopes:[…], serverVersion }` |
| `noa.listPanels` | read | `{}` | `{ panels:[Panel] }` |
| `noa.getText` | read | `{ paneId, source:"screen"|"scrollback", maxBytes? }` | `{ paneId, text, truncated? }` |
| `noa.getGrid` | read | `{ paneId, startRow, rowCount }` | `{ paneId, cols, startRow, coordinateGeneration, oldestRow, nextRow, rows:[Row], hasMore }` |
| `noa.sendText` | input | `{ paneId, text }` | `{ ok:true }` |
| `noa.focusPane` | control | `{ paneId }` | `{ ok:true }` |
| `noa.newTab` | control | `{ windowId? }` | `{ paneId }` |
| `noa.split` | control | `{ paneId, direction:"horizontal"|"vertical" }` | `{ paneId }` |
| `noa.closePane` | control | `{ paneId }` | `{ ok:true }` |
| `noa.subscribe` | read | `{ events:["state_changed","output"], paneIds?:[…] }` | `{ subscriptionId }` |
| `noa.unsubscribe` | read | `{ subscriptionId }` | `{ ok:true }` |
| notification `noa.stateChanged` | — | `{ panels:[Panel] }` (delta) | (notification, no response) |
| notification `noa.output` | — | `{ paneId, coordinateGeneration, lines:[Row], dropped?:true }` | (notification, no response) |

### ID model & Panel metadata

- IDs (`windowGroupId` / `windowId` / `paneId`) are all internal u64 values represented as **base-10 strings** in the API (since they can exceed JS's 53-bit safe integer range).
- The hierarchy is windowGroup (logical window) → window (native tab) → pane. `noa.newTab` **creates a tab plus its initial pane and returns the initial pane's `paneId`** (a tab can hold multiple panes via `noa.split` — it is not assumed to be 1:1).
- `Panel` = `{ windowGroupId, windowId, paneId, name, cwd, branch, process, busy, attention, preview }` — mirrors `SessionCard` (reusing sidebar extraction). `preview` includes color runs.

### Grid payload

- `Row` = `{ row, spans:[{ text, fg?, bg?, attrs? }] }`. `fg`/`bg` are `#rrggbb` or a palette index; `attrs` is a flag set for bold/italic/underline etc. **Consecutive cells with identical style are folded into a single span while preserving their text** (equivalent to PreviewSpan).
- Pagination: row range specified via `startRow`/`rowCount`. `Row.row`, `startRow`, `oldestRow`, and `nextRow` are session-absolute coordinates within one `coordinateGeneration`. Clients must discard cached rows when the generation changes. When the response would exceed the limit (roughly 256KB by default), it returns `hasMore:true` and defers the remainder to the next request. A single request that exceeds the limit outright is rejected with `-32005`.

### Push pipeline

- output: dirty rows are collected via a post-feed tap in `feed.rs` (O(1), no extra locking), coalesced at ≥16ms intervals, then delivered with `try_send` to a per-subscription bounded broadcast channel. When full, the oldest entry is dropped and the next notification sets `dropped:true`. Line diffs reuse the same color-run logic as sidebar extraction.
- state_changed: sidebar extraction is reused against the `Arc<Mutex<Snapshot>>` updated in `about_to_wait`, delivering a diffed `panels` payload only when a change is detected.
- io_thread/main-thread only perform `try_send` into the broadcast channel; subscriber threads own serialization and sending (NFR-2).

### Config keys & token file

- `server-enable` (bool, default `false`) / `server-port` (u16, default `61771`) / `server-token` (string, auto-generated if omitted) / `server-scopes` (comma-separated subset of `read,control,input`, default `read`). Adding these follows the existing standard 5-location change (`noa-config/src/lib.rs` + `parser/overrides.rs`).
- Token file: `server-token` under the config directory (e.g. `$XDG_CONFIG_HOME/noa/server-token`), permissions `0600`, generated only on first run. If `server-token` is set via config, both generation and reading from the file are skipped.

### Error code table

| code | meaning | trigger |
|------|------|------|
| `-32001` | auth failure | token mismatch / method call before auth |
| `-32002` | unknown pane | nonexistent `paneId`/`windowId` |
| `-32003` | scope denied | call to a method requiring an ungranted scope |
| `-32004` | pane closed | panel disappeared mid-execution |
| `-32005` | payload too large | request/response exceeds the limit |
| `-32006` | version mismatch | `protocolVersion` major mismatch |

(`-326xx` is the JSON-RPC implementation-defined range. Standard `-32600`–`-32603` are used for parse errors/invalid requests/unknown methods/invalid params.)

### Crate placement & integration points

- New crate `noa-ipc` (consumed by `noa-app` in the DAG; no `wgpu`/`winit` dependency).
- Mutation injection: add IPC variants to `UserEvent` in `noa-app/src/events.rs` (reusing existing WriteText/SpawnTab/ClosePane/AppCommand, adding only what's missing) and dispatch via `EventLoopProxy` (same path as AppleScript).
- State reads: the `Arc<Mutex<Snapshot>>` exposed in `about_to_wait` in `noa-app/src/app.rs` (reusing the `AppStateSnapshot` projection from `macos_applescript.rs`).
- Output tap: insert an O(1) tap into the feed loop (`feed.rs`) in `noa-app/src/io_thread.rs`, feeding the broadcast.
- Config: `noa-config/src/lib.rs` + `noa-config/src/parser/overrides.rs`.

## L3 — Acceptance Criteria

- **AC-1 (FR-1):** With `server-enable=false` (default) at startup, `61771` is not listened on (verified via `lsof`/connection attempt refusal); setting it to true makes it listen.
- **AC-2 (FR-2):** The server binds only `127.0.0.1`; connections to a LAN IP are refused. The app continues to run even if bind fails.
- **AC-3 (FR-3):** After the first launch, the token file exists with `0600` permissions and non-empty content. When `server-token` is set via config, no file is generated and the specified value is used.
- **AC-4 (FR-5, NFR-1):** A connection that does not present a valid token is rejected with `-32001`, and no `read`/`control`/`input` method executes.
- **AC-5 (FR-6, NFR-1):** A client granted only `read` calling `noa.sendText`/`noa.focusPane` receives `-32003`, and no pty injection or focus change occurs.
- **AC-6 (FR-6):** A client without `input` calling `noa.sendText` gets `-32003`. Having only `control` also results in `noa.sendText` being rejected (input is a separate scope).
- **AC-7 (FR-4, FR-19, NFR-5):** An `noa.hello` with a mismatched `protocolVersion` major is rejected with `-32006`; `grantedScopes` is only returned on a match. Requests with unknown fields do not error and are ignored.
- **AC-8 (FR-7):** `noa.listPanels` returns all panels, each including `paneId` plus name/cwd/branch/process/busy/attention/preview. IDs are base-10 strings.
- **AC-9 (FR-8):** `noa.getText source=screen` returns only visible lines; `source=scrollback` returns the entire content (scrollback + visible screen), both matching actual terminal content. Content exceeding `maxBytes` is truncated preferring the tail with `truncated:true` set (no unbounded bulk dump ever occurs).
- **AC-10 (FR-9, NFR-4):** `noa.getGrid startRow/rowCount` returns only the specified row range with color runs, excluding out-of-range rows. A grid larger than one screen is paginated with `hasMore:true`, and a single response stays within the limit.
- **AC-11 (FR-10..13):** `focusPane`/`newTab`/`split`/`closePane` perform focus change/tab creation/split/close respectively, and creation operations return the new `paneId`. Specifying a nonexistent `paneId` or `windowId` returns `-32002`.
- **AC-12 (FR-14):** After a client with `input` granted calls `noa.sendText`, the text reaches the target panel's pty and is accepted by the shell.
- **AC-13 (FR-15, FR-16):** While subscribed to `state_changed`, a change in a panel's busy/attention/name delivers `noa.stateChanged` with the delta `panels`. After `unsubscribe`, nothing more is delivered.
- **AC-14 (FR-17):** While subscribed to `output`, updates to panel output deliver `noa.output` with color-run-annotated line diffs.
- **AC-15 (FR-18):** Operations against a disappeared panel return `-32004`; a payload exceeding the limit returns `-32005`.
- **AC-16 (NFR-2):** Under a 10MB/s, 60-second pty output flood, the drop in feed throughput (bytes/sec) caused by the presence of an `output` subscriber is **within 5%**. When a subscription channel overflows, the oldest entry is dropped and the next notification sets `dropped:true`.
- **AC-17 (NFR-2):** Under a condition of 1 stalled client that never reads responses plus a 10MB/s, 60-second pty output flood, the increase in **p99 main-thread frame time is ≤5% vs. baseline and ≤1ms**.
- **AC-18 (NFR-3):** The implementation does not depend on an async runtime such as tokio (does not appear in `cargo tree`), and runs with 1 thread per connection plus crossbeam channels.
- **AC-19 (NFR-4):** `getGrid` never bulk-dumps a huge scrollback, always returning bounded responses via pagination/range. Dirty coalescing interval is ≥16ms.
- **AC-20 (FR-6, NFR-1):** With `server-scopes` unset (default), even if `noa.hello` requests `control`/`input`, `grantedScopes` is only `["read"]`. With `server-scopes = read,input` set, `input` is granted but `control` is not.
- **AC-21 (FR-19, NFR-5):** Calling an unknown method (e.g. `noa.nonexistent`) returns `-32601`, and subsequent known-method calls on the same connection are processed normally (connection stays alive).

## Scope

**In-scope (v1):**
- Server lifecycle (`server-enable` gate / loopback bind / token auto-generation + `server-token` override).
- Handshake + `protocolVersion` exchange + Bearer token auth + 3-scope (`read`/`control`/`input`) authorization.
- RPCs: `listPanels` / `getText` / `getGrid` (row-range pagination, with color runs) / `sendText` / `focusPane` / `newTab` / `split` / `closePane`.
- Subscriptions: `subscribe`/`unsubscribe` → `stateChanged` + `output` (color-run line diffs, `dropped` marker).
- Config `server-enable`/`server-port`/`server-token`/`server-scopes` (default `read` only), NFR-1..5, `noa-ipc` crate.

**Out-of-scope (v1):**
- Direct LAN bind / in-process TLS (iOS terminates via tunnel) / UDS transport (mentioned only as a degrade-path investigation topic for OQ-1).
- Full keybind vocabulary passthrough / per-client subscription policy.
- Raw VT stream delivery / PTY attach / CRDT/offline sync / multi-noa remoting.
  - **Amendment (2026-07-14):** raw VT delivery / PTY attach is reintroduced as an **additive extension** by `docs/specs/noa-client-mode.md` (`noa.attach` + dedicated attach channel + new `attach` scope), scoped to the noa↔noa Client Mode consumer. The original rejection rationale — thin clients must not reimplement VT interpretation — does not apply when the client is a full noa instance carrying the identical `noa-vt`/`noa-grid` engine. Dashboard/iOS-class clients remain on the structured-diff surface; nothing in this spec's shipped surface changes.

## Open Questions

- **OQ-1 (sandbox bind):** whether loopback TCP bind is permitted under sandbox execution is unverified (Omen ⑦). If not possible, the degrade path (e.g. UDS fallback) will be confirmed/decided during the implementation phase.
- **OQ-2 (token revocation/rotation):** the token rotation/revocation flow (handling of a regeneration CLI/config reload) is undecided for v1. Operation begins with just initial generation + `server-token` override; rotation is deferred to v2 consideration.
- **OQ-3 (response size limit):** the per-response limit for `getGrid`/`getText` (tentatively 256KB) needs tuning based on measurement. Whether the `-32005` threshold becomes configurable is undecided.
- **OQ-4 (subscribe authorization granularity):** subscriptions start with a blanket `read` grant. Per-panel fine-grained subscription policy is out-of-scope (a future opt-in).

(Resolved: ASSUME-1/ASSUME-2 were settled by the CHALLENGE decision as loopback TCP + iOS tunnel termination, and promoted to FR-2/FR-5/NFR-1.)

## Decision Ledger (Nexus discretionary decisions, all reversible)

| ID | Decision | Rationale |
|----|----|------|
| DEC-1 | Dependency stack = sync tungstenite + thread-per-connection + crossbeam (tokio not adopted) | The codebase uses only std-threads + crossbeam. Avoids introducing an async paradigm |
| DEC-2 | `getGrid` pagination = row-range unit | The grid's natural unit; simpler than tiles |
| DEC-3 | `server-port` default = fixed value 61771 | A discovery-file approach is deferred to v2 consideration |
| DEC-4 | Scope-granting mechanism = `server-scopes` config key (default `read` only) | Resolves Quality Gate blocker. Consistent with the explicit input opt-in decision |
| DEC-5 | `getText` truncates preferring the tail via `maxBytes` (default 256KB) + `truncated` | Resolves Quality Gate minor issue. Consistent with NFR-4 |

## Assumption Ledger

| ID | Content | Status |
|----|----|------|
| ASSUME-1 | Explicit out-of-scope items (remote access / pixel sharing / skipping auth) were unanswered by the user → undecided | resolved (CHALLENGE decision; see Scope section) |
| ASSUME-2 | Given iOS client requirements, limiting to localhost/unix-socket may not hold; whether network listening + auth is required was unconfirmed | resolved (loopback TCP + iOS tunnel; FR-2/FR-5) |

## EXPAND — Candidate directions (Riff + Flux, 2026-07-11)

- **A. Control-mode line protocol** (tmux/kitty-style): Unix socket + line-oriented text, push via `%` notifications. Fastest, zero dependencies, but has a custom-framing maintenance tax and manual typing work for iOS.
- **B. JSON-RPC over WebSocket** (LSP/DAP-style): unifies request/response and subscription push under methods + server notifications. Straightforward for iOS/dashboards. New dependencies on serde+tokio+ws.
- **C. gRPC/Protobuf mux server** (wezterm-style): server-streaming is first-class. Strongest typing, oriented toward remote-windows, but heaviest weight (tonic/protoc/certificates).
- **D. HTTP REST + SSE:** reads/actions = REST, push = SSE. Maximizes client reachability but weak for high-frequency bidirectional traffic.
- **E. PTY attach approach** (Flux: "The terminal IS the wire"): expose a pane as a detachable PTY; the client receives the raw VT byte stream and renders it itself (plus returns input/winsize). Push comes free via the pty read loop itself. iOS = a thin VT renderer. Risk: duplicated rendering path, auth/backpressure on the raw byte stream.
- **F. CRDT session document** (Flux): replicate panel state as a document synced across all peers. Offline viewing and multi-device support fall out naturally, but has the heaviest dependency weight and the most ambitious design.

## History

- 2026-07-11 FRAME: problem statement confirmed. Moving to EXPAND.
- 2026-07-11 EXPAND: candidates A–F generated (Riff A-D / Flux E-F).
- 2026-07-11 CHALLENGE entry: user singly selected **B (JSON-RPC over WebSocket)**. Browsing depth = metadata+preview / full-screen text / grid with color and attributes (raw VT streaming rejected).

## CHALLENGE — Stress test results (2026-07-11)

**Void+Magi (scope):** Trimmed from v1: TLS, token auth (if a UDS approach were used), TCP listening, per-client subscription policy, and full keybind vocabulary passthrough. Essential = `list_panels` / full-text retrieval / `send_text`+`focus_pane` / WS push (output+state_changed).

**Ripple (feasibility):** new crate `noa-ipc` (tentative). Reusing the existing 2 seams (`AppStateSnapshot`+`about_to_wait` exposure / `EventLoopProxy<UserEvent>` injection) gives low-to-medium blast radius. Dependencies are **sync tungstenite + thread-per-connection + crossbeam** (tokio rejected for introducing an async paradigm). Output push uses a post-feed tap in `feed.rs` (O(1), no extra locking); state reuses the extraction logic in `sidebar.rs`. Serialization coalesces dirty state at ≥16ms and is bounded by viewport/range.

**Omen (pre-mortem, in RPN order):** ① unauthenticated LAN bind = input-injection RCE (432) ② output flood melts io_thread/client (245) ③ slow-client backpressure stalls the terminal (200) ④ proxy flood (180) ⑤ large-scrollback serialization jank (150) ⑥ version skew with iOS (120) ⑦ bind impossible under sandbox (63).

**NFR candidates (must be in the spec):**
1. Default loopback/UDS-only bind + authorization on every method. Input/mutation are opt-in scopes separate from read.
2. The terminal/io_thread must never wait on a client — bounded `try_send`, drop-oldest + a missed-data marker.
3. No async runtime — sync tungstenite + thread-per-connection (same concurrency model as io_thread).
4. Serialization is bounded — limited to viewport/range, pagination required, dirty coalescing ≥16ms. Bulk full-scrollback dumps prohibited.
5. The protocol is versioned and additive-only — `protocolVersion` at handshake, rejected on major mismatch.

## CHALLENGE — Final decisions (user ruling, 2026-07-11)

| Issue | Decision |
|------|------|
| Transport/auth | **WS over loopback TCP (127.0.0.1) + mandatory token**. iOS via tunnel (SSH/Tailscale, etc.). Direct LAN exposure is a v2 opt-in |
| Color/attribute grid | **Included in v1** (viewport-limited, pagination required. Overrides Void's recommendation to defer to v2) |
| Output push format | **Structured line/preview diffs** (reusing sidebar extraction, no client-side VT interpretation needed) |
| Action vocabulary | **Minimal 5**: focus_pane / new_tab / split / close_pane / send_text |
| Dependency stack | **sync tungstenite + thread-per-connection + crossbeam** (DEC-1, technical decision: tokio not adopted) |

## Considered but rejected (EXPAND screening)

- A. Control-mode line protocol — custom-framing maintenance tax, unsuited to typed clients (iOS).
- C. gRPC mux — heavyweight (tonic/protoc/mTLS), overkill for near-term needs.
- D. REST+SSE — weak for high-frequency bidirectional traffic, two separate mental models.
- E. PTY attach — raw VT delivery rejected (requires client-side VT interpretation). *(Amendment 2026-07-14: revived for the noa↔noa Client Mode consumer only — see `docs/specs/noa-client-mode.md`; the rejection stands for thin/typed clients.)*
- F. CRDT document — too ambitious, dependency weight too high. Only the "scrollback=log/viewport=snapshot" state-splitting idea was borrowed for the design.
