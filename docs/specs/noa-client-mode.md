# noa Client Mode — Specification

- **Slug:** noa-client-mode
- **Title:** noa Client Mode — tab-integrated raw VT attach to a remote noa-server
- **Status:** locked (2026-07-14)
- **Owner:** simota
- **Build-path:** apex (autonomous end-to-end: design → risk gate → implementation loop → AC verification → ship. Place the SD-4 noa-grid reply-suppression flag design spike at the very start of the implementation phase.)
- **Supersedes:** amends `docs/specs/noa-server.md`'s Out-of-scope entry "Raw VT stream delivery / PTY attach" and Considered-but-rejected entry "E. PTY attach" as an additive extension, scoped to the noa↔noa consumer (amendment notes added to the parent spec, 2026-07-14). The original rejection rationale — thin clients must not reimplement VT interpretation — does not apply when the client is a full noa instance carrying the identical `noa-vt`/`noa-grid` engine. Dashboard/iOS-class clients remain on the structured-diff surface.

## L0 — Vision

**Problem:** noa can only operate panes inside its own GUI. Users running multiple Macs rely on SSH plus separate tools (tmux attach etc.) to see noa sessions on a remote Mac, leaving them outside the local tab/sidebar experience. The existing `noa-server` (`docs/specs/noa-server.md`) push (`noa.output`) carries only color-run line diffs — no VT state (cursor position, alternate screen, DEC private modes) — so remote vim/TUI apps cannot be operated as-is.

**Audience (who):** noa users (multi-Mac operators, remote developers). The target is another Mac on the LAN running the existing `noa-server` (with `server-bind` configured) or reached via an SSH/Tailscale-style tunnel. noa itself does not establish tunnels.

**Job-to-be-done:** display a noa session running on a remote Mac as a local noa tab/pane and type into it, tmux-attach style.

**Success:** the connect → show pane → type → see output loop runs at practical latency (key-input round-trip echo over LAN is not perceptible as lag), and remote shell operation — including vim/TUI — just works.

## L1 — Requirements

### Functional (FR)

- **FR-1 (control-plane connection):** the client establishes the existing `noa-server` JSON-RPC over WebSocket connection (`noa.hello` + Bearer token) and can request the new `attach` scope. `attach` is independent of `read`/`control`/`input` and is granted only when explicitly listed in `server-scopes` (an additive addition to the existing FR-6 three-scope model).
- **FR-2 (attach initiation):** `noa.attach` (scope `attach`) is called with a target `paneId` and returns connection information for the dedicated second channel (endpoint + one-time correlation token). A second attach to the same `paneId` is rejected with `-32007` (attach conflict) — single-attach model, avoiding winsize contention.
- **FR-3 (attach channel establishment):** the client opens the dedicated second WebSocket connection using the `noa.attach` response. This channel is not JSON-RPC: it carries raw PTY byte streams (no JSON/base64 framing) in both directions. If the handshake does not complete: `-32008` (attach handshake failure).
- **FR-4 (synthetic seed):** after attach-channel establishment, the server generates and sends a **synthetic repaint VT byte sequence (seed)** from the target pane's current grid. The seed is split into ordered binary messages of at most 64 KiB and terminated by one empty binary message, so it never depends on the 256 KiB WebSocket frame ceiling. The seed MUST include: visible grid content (with SGR attributes), cursor position/shape/visibility, saved cursor (DECSC), scroll region (DECSTBM), alternate-screen state, ANSI LNM, DEC private modes (including bracketed paste / mouse reporting / DECAWM autowrap / DECOM origin / DECCKM cursor keys / DECLRMM), each screen's horizontal margins (DECSLRM), tab stops, and charset designation (SCS). Scrollback is excluded from the seed and fetched separately via FR-12 lazy backfill. The window title is carried by `Panel` metadata and is not part of the seed.
- **FR-5 (ordering guarantee):** until seed transmission completes, the server holds any new live output bytes for the target pane in an internal buffer, then delivers buffered bytes followed by the live stream (preventing VT-parser desync from live bytes arriving ahead of the seed). **Raw-tap registration and the seed grid snapshot are taken atomically under a single Terminal lock acquisition** — structurally eliminating silent loss of bytes that would otherwise land in neither the seed nor the buffer (Judge B-2).
- **FR-6 (lossless delivery):** attach-channel output delivery does not reuse the existing `PushQueue` (drop-oldest 256, loss-tolerant); it uses a dedicated blocking/backpressured path. Byte-level loss is not permitted (a dropped byte permanently desyncs the VT parser).
- **FR-7 (client-side Terminal):** the client feeds received attach-channel bytes into a local `noa_vt::Stream`, updating a `noa-grid` `Terminal` (same type as existing local panes). This Terminal is displayed in a tab through the existing `FrameSnapshot`/renderer path unchanged.
- **FR-8 (reply suppression):** the FR-7 client-side Terminal must not forward reply bytes generated via `take_pending_writes()` (DA/DSR etc.) to the attach channel — reply authority belongs solely to the server-side Terminal. A reply-suppression flag is added to `noa-grid` (requires a design spike before implementation starts, SD-4).
- **FR-9 (input stream):** user key/mouse input is sent as a one-way raw input byte stream on the attach channel, not via `noa.sendText` (request/response RPC). Arrow keys, Ctrl sequences, mouse reports, and Escape timing are forwarded byte-faithfully. If the bounded client command queue saturates, the current attach generation is disconnected and later input is rejected until reconnect; a byte may never be silently dropped while the same stream remains active.
- **FR-10 (resize):** `noa.resizePane` (scope `attach`) lets the client change the remote pty window size of the target pane. The same grid-first discipline as existing `noa.newTab` etc. (resize the server-side grid first, then send winsize to the pty) is maintained on the remote side. Contention with a manual resize of the same pane by a GUI user on the server Mac is **last-writer-wins**; v1 has no arbitration mechanism (negligible harm in the primary use case of a single user attaching to their own second Mac; multi-user arbitration is out of scope).
- **FR-11 (remote pane creation):** the client issues existing control-plane methods (`noa.newTab`/`noa.split`, scope `control`) to create a new pane remotely, then calls `noa.attach` on the returned paneId. No Client-Mode-specific creation API is added.
- **FR-12 (scrollback lazy backfill):** for an attached pane, the client calls the existing `noa.getText` (`source=scrollback`) / `noa.getGrid` with scope `read` to backfill scrollback. Attach establishment itself does not wait for scrollback retrieval. Because this snapshot uses a separate WebSocket, the client caches it without merging until a suffix of the snapshot overlaps the local seed/raw-stream prefix; an unmatched snapshot is retried after later raw output rather than prepended speculatively.
- **FR-13 (reconnect):** if the attach channel disconnects, the client automatically reconnects with bounded backoff (**initial 1s, exponential, cap 30s, max 10 attempts**), performing `noa.attach` → new channel establishment → seed re-fetch. The tab is not closed; it shows a "reconnecting" badge, and after the max attempt count is exceeded, auto-retry stops and the tab transitions to a manual-retry state. The backoff state machine has an injectable Clock seam for deterministic testing.
- **FR-14 (detach/close):** closing the tab performs only subscription teardown via `noa.detach` (scope `attach`) and does not kill the remote process.
- **FR-15 (session-restore):** an additive `remote` field is added to noa's session-restore schema; remote tabs are restored as "disconnected remote tabs". Restoring must never silently respawn them as local shells.
- **FR-16 (explicit local-only for auto-approve/process display):** for remote panes, `process` metadata and auto-approve are out of scope in v1 and are displayed as an **explicit unsupported state** (e.g. `process=None`) — not a silent no-op.
- **FR-17 (non-loopback warning):** when the server the client is about to connect to is a non-loopback address and the requested scopes include `attach`/`input`, the UI shows an active warning before connecting.
- **FR-18 (connection UX):** connection settings live in config keys (the `client-*` family) plus a single "Attach Remote" command-palette entry. No dedicated connection-settings UI (GUI form) is provided in v1.
- **FR-19 (protocol compatibility):** all attach-related additions (`noa.attach`/`noa.detach`/`noa.resizePane`, scope `attach`, error codes `-32007`/`-32008`) follow the additive-only policy of the existing `noa-server.md` FR-19. `protocolVersion` stays at the existing value `1` (not a breaking change). Unknown-method/unknown-field handling is identical to the existing protocol.

### Non-Functional (NFR)

- **NFR-1 (latency):** noa-side added processing on the attach channel (input byte receipt → channel write; received byte → `Stream` feed) is under **1ms** each (a measurable budget). Overall feel is network-RTT-dominated; the judgment that vim over a loopback attach is perceptually indistinguishable from a local pane is made by manual GUI verification.
- **NFR-2 (losslessness):** attach-channel output delivery permits zero byte loss between server and client (FR-6). Designs that can drop (drop-oldest push) are not reused.
- **NFR-3 (concurrency model):** the client-side receive thread depends on no async runtime such as tokio. It follows the existing `io_thread` pattern (sync tungstenite + dedicated thread + crossbeam-channel bridging to `Arc<Mutex<Terminal>>` and the event loop).
- **NFR-4 (additive-only protocol):** all protocol additions in this spec are additive-only and do not bump the `protocolVersion` major. Backward compatibility with existing clients/servers is preserved.
- **NFR-5 (single-attach guarantee):** the server rejects multiple attaches to the same paneId (`-32007`), structurally preventing winsize ownership contention.
- **NFR-6 (server resource bounds):** attach channels (second WS connections) **count toward** the existing max concurrent connection limit (32) of `noa-server-protocol.md`. Non-loopback peers additionally share a per-source-IP limit of 27 connections, sized for one full nine-pane client tab at the peak of control + raw attach + scrollback backfill usage. There is no separate attach-only pool.

## L2 — Detail

### (a) noa-ipc protocol extension

**Scope table addition** (corresponding to `docs/api/noa-server-protocol.md` §4):

| Scope | Methods |
|---------|-------------|
| `attach` | attach / detach / resizePane |

`attach` is added as an additive fourth option to the `server-scopes` config (the existing comma-separated set of `read,control,input`). It is not part of the default `server-scopes=read`, so it is never granted unless explicitly allowed.

**Methods:**

`noa.attach` — requires `attach`
```json
→ {"jsonrpc":"2.0","id":10,"method":"noa.attach","params":{"paneId":"3"}}
← {"jsonrpc":"2.0","id":10,"result":{"attachToken":"<opaque>","attachUrl":"ws://127.0.0.1:61771/attach"}}
```
`attachUrl` is an attach-only endpoint reusing the existing server bind/port. **Routing mechanism:** keep the existing single `TcpListener` and branch on the WS-upgrade request path (`/` = existing JSON-RPC, `/attach` = attach channel) in `tungstenite`'s `accept_hdr` callback (no extra listener, no extra port; this is also what makes NFR-6's "counts toward the existing 32-connection cap" hold naturally in a single accept loop). `attachToken` is a one-time correlation token for that attach attempt (presented via header or the first frame when connecting the second channel).

`noa.detach` — requires `attach`
```json
→ {"jsonrpc":"2.0","id":11,"method":"noa.detach","params":{"paneId":"3"}}
← {"jsonrpc":"2.0","id":11,"result":{"ok":true}}
```
Subscription teardown only. The remote process is not killed (FR-14).

`noa.resizePane` — requires `attach`
```json
→ {"jsonrpc":"2.0","id":12,"method":"noa.resizePane","params":{"paneId":"3","cols":120,"rows":40}}
← {"jsonrpc":"2.0","id":12,"result":{"ok":true}}
```
Maintains the grid-first discipline (resize the server-side grid first, then send winsize to the pty).

**Attach channel handshake (second WS connection):**
1. The client connects via WS to `attachUrl` and presents `attachToken` in the first frame.
2. The server validates the token and, on success, sends the seed as ordered binary messages of at most 64 KiB followed by an empty binary seed terminator (FR-4/FR-5).
3. From then on the channel carries raw byte messages (binary WS messages) in both directions — server→client is PTY output, client→server is input bytes — not JSON-RPC. Large output and input are split into ordered messages of at most 64 KiB.
4. On disconnect, the client re-runs `noa.attach` to reconnect (FR-13).

**Error code table addition** (corresponding to `docs/api/noa-server-protocol.md` §8):

| code | meaning | trigger |
|------|------|------|
| `-32007` | attach conflict | second attach attempt to the same paneId |
| `-32008` | attach handshake failure | attachToken mismatch, timeout, channel establishment failure |

### (b) Server side (noa-app io_thread)

- Alongside the existing `IpcOutputTap` (the O(1) tap in `feed.rs`), add a new **raw tap** for attach-target panes. It is active only while an attached pane exists, and duplicates raw bytes independently of the existing color-run diff generation path.
- Queue policy for lossless delivery (FR-6): a dedicated blocking channel with timeout; on timeout, **disconnect and defer to client-side reconnect** (disconnect-on-overflow, not drop-oldest). This keeps backpressure from stalling the whole io_thread while guaranteeing that delivered bytes are never lost.
- Seed generator: assembles a synthetic VT byte sequence from the target pane's current grid (all states enumerated in FR-4). It reuses existing `Screen`/`Terminal` state readout, and the generated byte sequence must round-trip through the `noa_vt` parser (see the L3 ACs).
- **Atomicity (FR-5):** raw-tap registration and the seed grid snapshot happen under a single Terminal lock acquisition. For testing, provide a deterministic synchronization seam (test-only barrier) that can pause seed transmission → inject live bytes → resume.

### (c) Client side

- New module (in `noa-app`, positioned as a sibling of the existing `io_thread.rs`): the attach connection manager. A dedicated thread owns the second WS connection, feeds received bytes into `Arc<Mutex<Terminal>>` via `noa_vt::Stream`, and pokes the main thread with `UserEvent::Redraw` (same pattern as the existing `io_thread`, NFR-3).
- Add a transport enum `Local | Remote` to `Surface`. `Remote` holds the I/O paths to the attach connection manager (do not overload the meaning of the existing `io_thread: None`, per the Ripple finding).
- Input path: at `write_pane_pty_bytes` (`app/input_ops/terminal.rs`), the existing branch point from the key handlers, route to a raw attach-channel write when the `Surface` transport is `Remote`.
- Resize path: from the existing resize handler (`on_resize`, grid-first), add a branch issuing a `noa.resizePane` call for `Remote`.
- Reconnect state machine: `Connected` → (disconnect detected) → `Reconnecting(n)` (initial 1s, exponential, cap 30s, attempt n) → back to `Connected` on success, or `Detached` (manual-retry wait) after 10 attempts. The tab UI renders this state as a badge. The state machine has an injectable `Clock` seam (trait) (FR-13, for deterministic tests — no mockable clock exists in the codebase today, so it is introduced together with this feature).

### (d) noa-grid reply-suppression flag (design spike required)

- A mechanism is needed so that replies the client-side replica `Terminal` generates for queries such as DA (Device Attributes) / DSR (Device Status Report) are never forwarded to the attach channel, even when drained via `take_pending_writes()`.
- Whether to add a flag (it does not exist today) and where it lives (`Terminal` struct vs. the `Handler` implementation side) is deferred to a design spike before implementation starts (this spec fixes the requirement only, not the API shape: FR-8).

### (e) Config keys

Add a `client-*` family paired with the existing `server-*` keys (`noa-server.md` L2). Additions follow the standard five-site change pattern (`noa-config/src/lib.rs` + `parser/overrides.rs`).

| key | type | default | description |
|-----|------|---------|------|
| `client-remote` | string (`host:port`) | unset | Address of the target noa-server. Can also be overridden ad hoc from the "Attach Remote" command-palette flow |
| `client-token` | string | unset | Bearer token used for the connection (plaintext directly in config) |
| `client-token-file` | string (path) | unset | Path to read the token from a file. If `client-token` is set it takes priority and the file read is skipped (following the existing `server-token` precedence pattern) |

### (f) UI

- **Tab badge states:** `Connected` (normal display, remote-identity badge only) / `Reconnecting` (reconnecting indicator + attempt count) / `Detached` (disconnected, with a manual-retry affordance).
- **Command palette flow:** select "Attach Remote" → (prompt for the endpoint if `client-remote` is unset) → pick an attachable target pane (from `noa.listPanels`, whose `Panel.attachable` capability is derived from the server's actual raw-endpoint registry; unsupported panels remain visible but disabled), or create a new one via `noa.newTab`/`noa.split` → start the attach sequence. Remote sidebar cards consume connection-state and output redraws so their status and preview track the attached terminal.
- **Non-loopback warning copy (placeholder):** "The endpoint is not loopback. Make sure you are connecting over a trusted network." (FR-17; final copy tracked in Open Questions).

## L3 — Acceptance Criteria

- **AC-1 (FR-1):** requesting `attach` via `noa.hello` against a server whose `server-scopes` does not include `attach` yields `grantedScopes` without `attach`. Calling `noa.attach` without the scope returns `-32003` (scope denied).
- **AC-2 (FR-2):** a second `noa.attach` on the same `paneId` returns `-32007` and the existing attach remains intact.
- **AC-3 (FR-3):** the attach channel carries no JSON-RPC frames whatsoever, only raw bytes (assert frame contents in a protocol-level test harness; packet capture is a fallback method). A mismatched attachToken yields `-32008` (integration test).
- **AC-4 (FR-4):** generate a seed from a fixture grid configured with every state enumerated in FR-4 (cursor position/shape/visibility, saved cursor, scroll region, alternate screen, SGR attributes, LNM, bracketed paste/mouse/DECAWM/DECOM/DECCKM/DECLRMM, primary/alternate DECSLRM, tab stops, charset designation); feeding the seed and subsequent live bytes into the `noa_vt` parser reproduces those states and behavior of the source grid (round-trip unit test).
- **AC-5 (FR-5):** using the test-only synchronization barrier (pause seed transmission → inject live bytes → resume) in a deterministic integration test, verify (a) raw-tap registration and the seed snapshot happen under a single Terminal lock, and (b) the client-side final state matches the seed → buffered bytes → live ordering with no loss.
- **AC-6 (FR-6):** apply deterministic fault injection to attach-channel output delivery (intentionally stall the receiver while sending a fixed number of bytes at a fixed rate) and verify that delivered bytes have zero loss, and that a stall exceeding the timeout results in disconnection (disconnect-on-overflow), never drop-oldest (integration test; a 10MB/s-class load harness is not required as none exists).
- **AC-7 (FR-7):** a remote pane displayed via attach renders alternate-screen transitions, cursor movement, and color attributes of TUI apps such as vim correctly, through the same renderer path as local panes (manual GUI verification).
- **AC-8 (FR-8):** feed a DSR/DA query sequence into a client-side Terminal with the reply-suppression flag enabled and verify that the byte log of a mock writer capturing attach-channel writes contains no reply bytes (unit test; depends on the SD-4 flag implementation).
- **AC-9a (FR-9):** each input event — arrow keys, Ctrl+C, mouse reports — is written to the attach channel (mock writer) byte-for-byte identical to the local pane's input encoding (unit test).
- **AC-9b (FR-9):** real keystrokes and mouse actions on a remote pane produce correct reactions from the remote shell/TUI (manual GUI verification).
- **AC-10 (FR-10):** resizing the local window on an attached pane issues `noa.resizePane` and the remote pty winsize is updated. The grid-first order (local grid update → RPC dispatch) is preserved (integration test).
- **AC-11 (FR-11):** choosing new-remote-pane creation from the command palette issues the existing `noa.newTab`/`noa.split`, and `noa.attach` runs automatically on the returned `paneId`; a discovered `Panel` with `attachable:false` is disabled and cannot dispatch an attach request (integration/unit tests).
- **AC-12 (FR-12):** verify by integration test that attach establishment completes without waiting on a scrollback RPC round-trip (call-order assertion: at seed delivery completion, the `getText` call may be un-issued or incomplete), and that a snapshot containing not-yet-received raw output is not merged until later raw bytes establish an overlap boundary; the older prefix is then merged exactly once.
- **AC-13 (FR-13):** after attach-channel disconnection, the client retries with the default backoff (initial 1s, exponential, cap 30s, max 10 attempts) and, on reaching the max count, stops auto-retry and transitions to manual-retry wait — verified deterministically using the injected Clock seam (integration test). The tab badge's state transitions are verified manually in the GUI.
- **AC-14 (FR-14):** closing an attached remote tab issues only `noa.detach` and the remote pane's process does not terminate (integration test with remote process liveness check).
- **AC-15 (FR-15):** restarting the app with a remote tab present restores it as a "disconnected" remote tab and never silently respawns it as a local shell (manual GUI verification).
- **AC-16 (FR-16):** the sidebar/process field of a remote pane shows an explicit unsupported indication (e.g. empty `process` field / dedicated icon) and the pane is excluded from auto-approve. Its card switches among Connected/Reconnecting/Detached status and connected raw output updates the colored preview through `SessionDelta` (unit test plus manual GUI verification).
- **AC-17 (FR-17):** with `client-remote` set to a non-loopback address and requested scopes including `attach` or `input`, attempting to connect shows the UI warning before connection establishment (manual GUI verification; execution pending OQ-4 trigger-condition resolution).
- **AC-18 (FR-19, NFR-4):** after the attach RPC additions, the behavior of non-attach clients (`noa.listPanels` etc.) and the `protocolVersion` value (`1`) are unchanged (re-run and pass the existing `noa-server` protocol compatibility tests).
- **AC-19 (NFR-3):** no async runtime such as tokio appears as a dependency of the client-side attach receive thread (verified via `cargo tree`). Note: dependency absence is a necessary condition only; positive conformance to "sync tungstenite + dedicated thread + crossbeam" is confirmed by implementation review.
- **AC-20 (NFR-5):** with multiple concurrent `noa.attach` attempts on the same paneId, the later request is always rejected with `-32007` and no double ownership of winsize occurs (integration test).
- **AC-21 (NFR-6):** with the total connection count — including attach channels (second WS connections) — at the existing max concurrent connection limit (32), a new `noa.attach` attempt behaves the same as the existing connection-cap error handling (connection refused) (integration test; reuse the existing `tcp_pair()` helper's connection-loop pattern).
- **AC-22 (FR-18):** with `client-remote`/`client-token` set in config, invoking the "Attach Remote" command-palette entry starts the attach sequence (integration test: drive the palette action handler directly). Also, no connection-settings GUI form is reachable from any menu/palette (manual GUI verification).
- **AC-23 (NFR-1):** measure that noa-side added processing (input byte receipt → attach channel write; received byte → `Stream` feed) is under 1ms each (unit/integration test). Manually verify in the GUI that vim over a loopback attach is perceptually indistinguishable from a local pane.
- **AC-24 (FR-6, NFR-2):** drive the attach channel with a synthetic load generator (in-process/loopback, fixed rate, sustained output at or above real TUI levels — guideline: 10MB/s for 5 seconds) and verify that, against a client that keeps receiving normally, disconnect-on-overflow never fires and all bytes are delivered in order with no loss (integration test; paired with AC-6's loss check, this guarantees both "never loses bytes" and "never disconnects at practical throughput").

## Scope

**In-scope (v1):**
- Full-VT-fidelity remote pane display and typing via a per-pane second WS connection (integrated into the existing tab UI).
- Additive extension `noa.attach`/`noa.detach`/`noa.resizePane` + new `attach` scope + new error codes `-32007`/`-32008`.
- Synthetic seed (including mode state) + live ordering guarantee + lossless delivery.
- Client-issued resize / bounded-backoff auto-reconnect with seed re-fetch.
- Remote pane creation (issuing existing control methods `noa.newTab`/`noa.split`) / scrollback lazy backfill (existing `noa.getText`/`noa.getGrid`).
- Additive `remote` field in the session-restore schema (restore as a disconnected remote tab).
- Explicit unsupported-state UI for remote panes (`None` in the process field, explicit exclusion from auto-approve, FR-16 — UI that is actively built, distinct from "not building process metrics").
- Active UI warning for non-loopback connection + `attach`/`input` scopes.
- Connection UX: config file (`client-*` keys) + a single "Attach Remote" command-palette entry.

**Out-of-scope (v1):**
- Simultaneous connections to multiple servers / multiple attaches to the same pane (single-attach model).
- mDNS auto-discovery / remote forwarding of kitty graphics / remote OSC52 clipboard sync.
- Connection-settings UI (GUI form). Direct config-file editing only.
- Process metrics display / auto-approve for remote panes (local-only in v1, explicitly shown as `process=None`).
- TLS termination (external tunnel assumed. noa itself does not establish tunnels. The UI only warns).

## Considered but rejected

- **C2 dedicated remote window + per-pane binary tunnel** — failure-class isolation is a real benefit, but the user chose C1 (tab integration), prioritizing the integrated tab/sidebar experience (EXPAND checkpoint, 2026-07-14).
- **C3 server-owned grid, client-as-renderer** — structurally strongest for multi-attach/mid-stream attach, but the permanent maintenance cost of a new wire schema plus a RemoteScreen render layer is heavy.
- **C4 hybrid (read view + attach-on-focus)** — best bandwidth profile, but the lifecycle complexity of two render paths. v1 prioritizes a single path.
- **Reusing sendText (input path)** — request/response RPC with timeouts cannot satisfy byte fidelity for arrow keys/Ctrl/mouse/Escape timing; a one-way raw input stream is introduced instead.
- **Reusing the existing PushQueue (output path)** — its drop-oldest design causes permanent VT parser desync; a dedicated lossless attach path is introduced instead.
- **Seed via full Terminal serialization** — a heavyweight serialization format is unnecessary; generating a synthetic VT byte sequence from the grid reuses the existing `noa_vt` parser as-is and stays lightweight.
- **Resize wiggle / view-only resize** — unified on client-authoritative resize without breaking the grid-first discipline (user's choice in OD-1).
- **tmux-style multiplexing intersection model (winsize = intersection of all attached clients)** — unnecessary given the single-attach model (NFR-5).
- **Manual retry only (no auto-reconnect)** — the user chose auto-reconnect with bounded backoff (OD-2).

## Open Questions / Deferred Decisions

- **OQ-1 (multiplexing fallback threshold):** the pane fan-out count at which attach channels switch from individual second WS connections to a binary-frame-multiplexed fallback on a single connection is undecided. To be determined from implementation/operational measurement.
- ~~OQ-2~~ **resolved (2026-07-14 quality gate):** method names `noa.attach`/`noa.detach`/`noa.resizePane`, error codes `-32007`/`-32008`, and the scope name `attach` are **final** as of this spec's LOCK (resolving the contradiction of ACs depending on provisional values; confirmed non-colliding with the existing `-32000..-32006` code range). Additional parameter fields may be extended at implementation time within the additive-only policy.
- **OQ-3 (session-restore remote field shape):** the concrete JSON shape of the `remote` field added by FR-15 (how to hold the endpoint, paneId, last-disconnect time, etc.) is undecided.
- **OQ-4 (non-loopback warning copy / trigger conditions):** the L2(f) warning copy is a placeholder. The exact display trigger (connect-time only, or also on config save) is undecided.
- ~~OQ-5~~ **resolved (2026-07-14 SPECIFY checkpoint):** attach-channel I/O is complete with the `attach` scope alone (no simultaneous `input` requirement). attach is self-contained as "full interactive control", keeping the scope design simple.
- **OQ-6 (tunnel warning false negative):** in the SSH/Tailscale tunnel-terminating-at-`127.0.0.1` configuration (the primary access pattern L0 assumes), FR-17's non-loopback warning does not fire even though an untrusted network is genuinely being crossed. In v1 this is documented as a known limitation (tunnel-endpoint detection is fundamentally hard). Docs must state explicitly: "when using a tunnel, the network between endpoints is assumed to be protected by the tunnel."

## Assumption Ledger

| ID | Item | Status |
|----|------|--------|
| ASSUME-1 | Whether v1 includes remote pane creation (newTab/split, control scope) or attach-to-existing only | resolved — in v1 scope (OD-3; only issues existing control methods, no Client-Mode-specific API, FR-11) |
| ASSUME-2 | Connection UX entry point (fixed config / command palette / UI) | resolved — config file (`client-*`) + a single "Attach Remote" command-palette entry (OD-3, FR-18) |

## Decision log

- **SD-1 (Transport):** raw VT delivery does not reuse the existing `PushQueue` (drop-oldest); it uses a dedicated lossless, backpressured path (per-pane second WS connection).
- **SD-2 (Input):** `sendText` not adopted. One-way raw byte input stream + a new dedicated `attach` scope.
- **SD-3 (Seed):** generate a synthetic repaint VT byte sequence from the server grid and inject it through the same `noa_vt::Stream` path as the live stream. Always include mode state; buffer live bytes server-side until the seed completes, guaranteeing ordering.
- **SD-4 (Reply suppression):** the client-side replica Terminal never forwards `take_pending_writes()` output to the attach channel. A reply-suppression flag must be added to noa-grid (design spike before implementation).
- **SD-5 (Security):** the UI actively warns on the combination of non-loopback bind and `attach`/`input` scopes. Token auth reuses the existing Bearer + scope model (no second auth system).
- **SD-6 (session-restore):** silently respawning remote tabs as local shells is forbidden. Restore as disconnected via an additive `remote` field.
- **SD-7 (auto-approve/process display):** remote panes are explicitly local-only in v1 (`process=None`, never a silent no-op).
- **SD-8 (Tab close):** closing a remote pane is subscription teardown only (the remote process is not killed).
- **OD-1 (resize):** the client may resize the remote pty (new `noa.resizePane` RPC). Grid-first discipline maintained.
- **OD-2 (reconnect):** automatic reconnection with bounded backoff + re-attach + seed reuse.
- **OD-3 (scope):** minimal v1 as the base, with two items restored: remote pane creation and scrollback lazy backfill. The other cuts stand (multi-server, multi-attach, mDNS, kitty graphics, OSC52, settings UI).
- **DEC-1 (interpretation):** in OD-3, "approve all cuts" and the two restored items were selected simultaneously; interpreted as "minimal v1 plus exactly those two items."
- **OD-4 (SPECIFY checkpoint):** attach second WS connections count toward the existing 32 max concurrent connections (no separate limit; NFR-6 approved).
- **OD-5 (SPECIFY checkpoint):** attach-channel I/O is complete with the `attach` scope alone (OQ-5 resolved). protocolVersion=1 unchanged and the `client-*` key names were also approved at this point.

## History

- 2026-07-14 FRAME: confirmed the problem statement and target-endpoint assumptions.
- 2026-07-14 Lens (Reuse/Constraint findings): judged the noa-ipc protocol types, auth, push subscriptions, the `write_pane_pty_bytes` branch point, the config-addition pattern, and tungstenite 0.24 as reusable. Judged raw VT delivery insufficient on the existing push machinery — an additive extension required.
- 2026-07-14 EXPAND: Riff×Flux presented C1-C4. The user chose C1 (raw-VT attach + existing tab integration).
- 2026-07-14 CHALLENGE: Magi/Void/Ripple/Omen produced SD-1..8 (agent consensus, no user ruling needed). Ripple issued a Conditional Go (blast radius 8-10 files; resolving SD-4 is the precondition for implementation start). The user ruled OD-1..3.
- 2026-07-14 SHAPE: Spark synthesized the Proposal.
- 2026-07-14 SPECIFY: Accord authored this document in L0-L3 form. The user ruled OD-4/OD-5.
- 2026-07-14 LOCK: user sign-off. Build path = apex. Promoted to this file (status: locked).
- 2026-07-14 Quality gate: Judge (BLOCKER 2 / MAJOR 7 / MINOR 3) + Attest (CONDITIONAL, AC verifiability audit). All findings fixed — B-1: amendment notes in the parent `noa-server.md` + Supersedes section here / B-2: single-Terminal-lock atomicity of tap registration + seed snapshot (FR-5, AC-5) / M-1→AC-22 / M-2→NFR-1 quantified + AC-23 / M-3→FR-4 state enumeration + AC-4 expansion / M-4→FR-10 last-writer-wins / M-5→FR-16 named in In-scope / M-6→backoff values fixed (1s/×2/30s/10 attempts) + Clock seam / M-7→attachUrl `accept_hdr` path branching stated in L2(a) / re-verification: 12 PASS + new M-8 (sustained-throughput verification) → resolved by adding AC-24 / m-1→OQ-2 resolved (names and codes final) / m-2→AC-3/AC-8 switched to harness/mock-writer methods / m-3→OQ-6 added. Attest: fixed the AC-3 FR-8 mis-mapping, made AC-5 a deterministic barrier test, made AC-6 fault-injection-based (independent of the unbuilt 10MB/s harness), split AC-9 into a/b, fully automated AC-12, and switched AC-13 to the injected-Clock approach.
