# noa-ipc pane subscriptions — design spec

- slug: `noa-ipc-pane-subscriptions`
- status: `proposed` (2026-11-21)
- resolves: OQ-4 in `docs/specs/noa-server.md`
- scope: `crates/noa-ipc` (protocol, push, server, client) and the `noa.stateChanged` diff in `crates/noa-app/src/app/ipc.rs`

## Problem

`noa.subscribe` can narrow a subscription to `paneIds`, but three gaps make a
per-pane subscriber poll `noa.listPanels` anyway:

1. A closed pane is silently absent from the next diff
   (`sync_ipc_snapshot`, comment above the `has_state_changed_subscribers`
   check: "a pane that disappeared ... has no removal event in v1").
2. Every field change is pushed. A waiter that only cares about `agent` gets a
   full `Panel` each time `preview` or `cwd` moves.
3. A subscription cannot be changed. Following a new worker means
   `unsubscribe` + `subscribe`, which opens a gap and changes the id.

## Resolution of OQ-4

OQ-4 asked for per-panel subscription granularity. This spec resolves it with
three additive changes to subscription scoping: removal notices, field filters,
and in-place update. Authorization stays as it is: `noa.subscribe`,
`noa.updateSubscription` and `noa.unsubscribe` require `read`, and no per-pane
ACL is introduced.

All changes are additive (FR-19). `PROTOCOL_VERSION` stays `3`.

## Non-goals

- No change to `noa.output` other than what `updateSubscription` does to its filter.
- No new consumer behaviour in `+ctl`. `crates/noa-app/src/ctl.rs` gets only the
  mechanical edits that keep it compiling (unit `ipc-client`).
- No merging or coalescing of queued `stateChanged` entries (see Risk R-2).

## AC-1 — Removal notice (`removedPaneIds`)

### Wire

`noa.stateChanged` params gain `removedPaneIds: [WireId]`, omitted when empty.
`panels` stays present on every notification (possibly `[]`), so a client that
only reads `panels` still parses it.

```json
{"jsonrpc":"2.0","method":"noa.stateChanged",
 "params":{"panels":[],"removedPaneIds":["13"]}}
```

```json
{"jsonrpc":"2.0","method":"noa.stateChanged",
 "params":{"panels":[{"paneId":"12","busy":false,"...":"..."}],"removedPaneIds":["13","14"],"dropped":true}}
```

### Semantics

- A pane id is in `removedPaneIds` when it was in the previous IPC listing, is
  absent from the current one, and its registry entry is gone after
  `IpcRegistry::prune`, meaning `shared.registry.resolve(id)` is `None` because
  its Surface closed. A pane that leaves the listing while its Surface is still
  alive is not reported. That case comes up rarely: a pane whose window stops
  being sidebar-eligible, or a float losing its parent. The subscriber sees it
  again as an added panel if it comes back, or learns of it from the next
  `listPanels`. A cross-tab move keeps its id (`IpcRegistry::rekey`), so it is
  neither removed nor added.
- Scope: a removed id is delivered to a connection when at least one of its
  subscriptions has `state_changed` and either no `paneIds` or a `paneIds`
  set that contains the id. `fields` never filters removals (AC-2).
- A notification is enqueued when the per-connection result has panels or
  removed ids. A delta that only removes panes still produces one.
- Order: `removedPaneIds` follows the previous listing's order, without duplicates.
- Pane ids are never reused within a server instance, so a subscription's
  `paneIds` set is not pruned after a removal.

### Code

- `crates/noa-ipc/src/protocol.rs`: `StateChangedParams` gets
  `#[serde(rename_all = "camelCase")]` and
  `#[serde(skip_serializing_if = "Vec::is_empty")] removed_pane_ids: Vec<WireId>`.
  `dropped` keeps its name, since camelCase leaves a single word unchanged.
- `crates/noa-ipc/src/push.rs`:
  - `QueuedNotification::StateChanged { panels, removed_pane_ids: Vec<u64>, dropped }`.
  - New `pub struct ChangedPanel { pub panel: Panel, pub fields: FieldMask }`.
    `fields` lists the fields that differ from the previous listing, and an
    added panel carries `FieldMask::ALL`.
  - New `Broadcaster::broadcast_state_delta(&self, changed: Vec<ChangedPanel>, removed: Vec<u64>)`.
    This becomes the one real implementation. `broadcast_state_changed(panels)`
    stays as a wrapper that passes `FieldMask::ALL` for each panel and no
    removals, so existing callers keep compiling without edits:
    `crates/noa-ipc/examples/bench_rpc.rs`, `crates/noa-ipc/tests/server_tests.rs`
    and `crates/noa-ipc/tests/orchestration_tests.rs`.
  - `PushQueue::drain` is unchanged except for the new variant field.
- `crates/noa-ipc/src/server.rs`: `to_notification` maps
  `removed_pane_ids` to `WireId`s in `StateChangedParams`.
- `crates/noa-app/src/app/ipc.rs`: `changed_panels(previous, current) -> Vec<Panel>`
  is replaced by
  `state_delta(previous: Vec<Panel>, current: &[Panel], live: impl Fn(u64) -> bool) -> StateDelta`,
  where `StateDelta { changed: Vec<ChangedPanel>, removed: Vec<u64> }` is a
  private struct in `app/ipc.rs` with `is_empty()`. The tail of `sync_ipc_snapshot` becomes:

  ```rust
  if !self.ipc_broadcaster.has_state_changed_subscribers() {
      return;
  }
  let delta = {
      let shared = self.ipc_shared.lock();
      state_delta(previous_panels, &shared.panels, |id| shared.registry.resolve(id).is_some())
  }; // ipc_shared guard dropped here, before any Broadcaster lock
  if !delta.is_empty() {
      self.ipc_broadcaster.broadcast_state_delta(delta.changed, delta.removed);
  }
  ```

  The stale comment ("has no removal event in v1") is rewritten.
  `crates/noa-app/src/ipc_bridge.rs` needs no change, because
  `IpcRegistry::resolve` already exists.

### Tests

- `push.rs` (inline): a removal in scope is delivered, and one out of scope is not;
  a removal-only delta enqueues exactly one notification with empty `panels`;
  `paneIds: None` receives every removal; two subscriptions on one connection
  give one notification with the union, which keeps R-5.
- `protocol.rs` (inline): `StateChangedParams` omits `removedPaneIds` when empty
  and serializes ids as decimal strings when present.
- `server_tests.rs`: over the wire, `broadcast_state_delta(vec![], vec![1])`
  reaches a subscriber as `{"panels":[],"removedPaneIds":["1"]}`.
- `app/ipc.rs` `diff_tests`: `state_delta` reports a pane absent from `current`
  with `live == false` as removed, and leaves out one with `live == true`. A pane
  listed twice in `previous` is reported once. The masks on changed panels are
  covered by the existing `the_diff_holds_new_and_changed_panels_in_listing_order`,
  ported to `state_delta`.

## AC-2 — Field filter (`fields`)

### Wire

```json
{"jsonrpc":"2.0","id":7,"method":"noa.subscribe",
 "params":{"events":["state_changed"],"paneIds":["12","13"],"fields":["agent","busy"]}}
→ {"jsonrpc":"2.0","id":7,"result":{"subscriptionId":"4"}}
```

```json
{"jsonrpc":"2.0","id":8,"method":"noa.subscribe",
 "params":{"events":["state_changed"],"fields":["agnt"]}}
→ {"jsonrpc":"2.0","id":8,"error":{"code":-32602,"message":"invalid params"}}
```

### Semantics

- Accepted names are exactly the `Panel` keys as serialized: `windowGroupId`,
  `windowId`, `paneId`, `name`, `cwd`, `branch`, `process`, `busy`,
  `attention`, `attachable`, `preview`, `surfaceId`, `agent`, `exit`,
  `parentPaneId`, `label`, `spawn`. `paneId` is the diff key and never changes,
  so a subscription with `fields: ["paneId"]` receives only additions and removals.
- With `fields`, a changed panel is delivered only if one of the named fields
  differs from the previous listing. An added panel (`FieldMask::ALL`) and a
  removed pane are always delivered. `fields: []` means additions and removals only.
- A delivered panel is always the full `Panel`, not a partial object, so
  `Panel` and its client parsing stay unchanged.
- `fields` applies only to `state_changed`. On an `output`-only subscription it
  is accepted and has no effect.
- Without `fields`, behaviour is unchanged: any field change delivers.
- On one connection, R-5 consolidation applies: a panel is in the single
  notification if any of the connection's subscriptions matches it by scope and fields.
- Field values change at the cadence they already have. `cwd`, `branch`,
  `process` and `preview` refresh on the coarse `IPC_SNAPSHOT_REFRESH` tick,
  not on the structural signature (`ipc_structural_signature`). A `fields`
  filter does not make them faster.

### Code

- `crates/noa-ipc/src/protocol.rs`:
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)] #[serde(rename_all = "camelCase")] pub enum PanelField { WindowGroupId, WindowId, PaneId, Name, Cwd, Branch, Process, Busy, Attention, Attachable, Preview, SurfaceId, Agent, Exit, ParentPaneId, Label, Spawn }`.
    An unknown string fails deserialization of the whole params, and
    `handle_subscribe` already maps that to `RpcFail::invalid_params()` (-32602).
  - `pub struct FieldMask(u32)` with `ALL`, `EMPTY`, `from_fields(&[PanelField])`,
    `intersects(self, other) -> bool` and `is_empty()`. Each bit is
    `1 << (PanelField as u32)`.
  - `impl Panel { pub fn changed_fields(&self, previous: &Panel) -> FieldMask }`.
    It destructures both panels without `..`, so adding a `Panel` field is a
    compile error until it is mapped to a `PanelField`.
  - `SubscribeParams` gains `#[serde(default)] pub fields: Option<Vec<PanelField>>`.
- `crates/noa-ipc/src/push.rs`:
  - `SubFilter` gains `fields: Option<FieldMask>`.
  - New `add_subscription_filtered(conn_id, events, pane_ids, fields: Option<FieldMask>)`.
    `add_subscription` delegates with `None`, so the callers in
    `crates/noa-app/src/io_thread/tests.rs` and in `app/ipc.rs` `diff_perf`
    stay untouched.
  - Per panel, `broadcast_state_delta` checks
    `sub.events ∋ STATE_CHANGED && in_scope(sub, pane) && sub.fields.is_none_or(|f| f.intersects(changed.fields))`.
- `crates/noa-ipc/src/server.rs`: `handle_subscribe` passes
  `p.fields.map(|f| FieldMask::from_fields(&f))`.
- `crates/noa-app/src/app/ipc.rs`: `state_delta` builds each `ChangedPanel` with
  `current.changed_fields(previous)`. It keeps the panel when the mask is
  non-empty, and uses `FieldMask::ALL` when there is no previous entry.

### Cost of the per-subscription field diff

The field diff is computed once per panel per sync in the app, not once per
subscription. `changed_fields` replaces the existing `previous != current`
comparison in `changed_panels`. For an unchanged panel the cost is the same:
`PartialEq` on equal values compares every field too. For a changed panel it
adds at most the rest of one field-by-field comparison, because it does not
stop at the first difference. In the Broadcaster each subscription costs one
`u32` AND per changed panel, under the `conns` lock that the existing
scope check already holds. The cost stays O(connections × subscriptions ×
changed panels), as now, with each step replaced by a bit test.
`publish_cost_with_twenty_panels` in `app/ipc.rs` (an ignored release-mode
test) is ported to `state_delta`, so the before and after numbers can be compared.

### Tests

- `protocol.rs`: serialize a `Panel` with every optional field set, take its
  object keys, and assert that they equal the set of `PanelField` names (the
  names are the wire names); each `PanelField` has a distinct bit; a JSON name
  that is not a field fails to deserialize; for each field, mutating only that
  field makes `changed_fields` return exactly that bit; equal panels give `EMPTY`.
- `push.rs`: with `fields: [agent]`, a panel whose mask is `{preview}` is not
  delivered, `{agent}` is, and `ALL` (added) is; a removal is delivered despite
  `fields: []`; with two subscriptions on one connection, one filtered and one
  not, the panel is delivered; `fields` on an `output` subscription does not
  change `broadcast_output`.
- `server_tests.rs`: `noa.subscribe` with `"fields":["agnt"]` returns -32602
  and creates no subscription (a later broadcast delivers nothing; probe with a
  `listPanels` round trip as `subscribe_delivers_state_changed_and_unsubscribe_stops_it` does).

## AC-3 — `noa.updateSubscription`

### Wire

| Method | Scope | params | result |
|---|---|---|---|
| `noa.updateSubscription` | read | `{ subscriptionId, events?, paneIds?, fields? }` | `{ ok:true }` |

Each optional key works the same way:

- absent: keep the current value;
- `null` (`paneIds`, `fields` only): clear the filter, meaning all panes or all fields;
- array: replace the value. `events: []` is accepted, as in `noa.subscribe`, and
  makes the subscription inert. `events: null` is -32602.

```json
{"jsonrpc":"2.0","id":9,"method":"noa.updateSubscription",
 "params":{"subscriptionId":"4","paneIds":["12","13","15"]}}
→ {"jsonrpc":"2.0","id":9,"result":{"ok":true}}
```

```json
{"jsonrpc":"2.0","id":10,"method":"noa.updateSubscription",
 "params":{"subscriptionId":"4","paneIds":null,"fields":null}}
→ {"jsonrpc":"2.0","id":10,"result":{"ok":true}}
```

```json
{"jsonrpc":"2.0","id":11,"method":"noa.updateSubscription","params":{"subscriptionId":"99"}}
→ {"jsonrpc":"2.0","id":11,"error":{"code":-32602,"message":"unknown subscription"}}
```

### Semantics

- Same limits as `noa.subscribe`: more than `MAX_SUBSCRIBE_PANE_IDS` (1024)
  `paneIds` is -32602, and an unknown `fields` name or `events` kind is -32602.
  An update does not count against `MAX_SUBSCRIPTIONS_PER_CONNECTION` (16),
  because it creates nothing.
- The id and its position on the connection are kept.
- **Error for an unknown id, or an id owned by another connection: -32602
  `InvalidParams`, message `"unknown subscription"`.** Both cases return the
  same answer, so a client cannot probe other connections' subscription ids.
  -32002 (`UnknownPane`) is about panes and does not fit. -32600 stays reserved,
  as in `handle_subscribe`, for the unreachable "connection not registered" case.
- Everything is validated before anything changes. A rejected update leaves
  the subscription exactly as it was.
- The update is applied under one `conns` lock acquisition, so any broadcast
  sees either the old filter or the new one, never a mix.
- Notifications enqueued before the update stay queued.
  `run_connection_loop` drains the queue at the top of each iteration, so they
  can reach the client after the `{ ok:true }` response. Notifications enqueued
  after the response use the new filter.
- Widening the scope sends no snapshot of the newly covered panes. As after
  `noa.subscribe`, the client lists them itself.
- `noa.unsubscribe` keeps returning `{ ok:true }` for an unknown id. That
  behaviour is not changed here.

### Code

- `crates/noa-ipc/src/protocol.rs`:
  `#[derive(Clone, Debug, Default, Serialize, Deserialize)] #[serde(rename_all = "camelCase")] pub struct UpdateSubscriptionParams { pub subscription_id: WireId, events: Option<Vec<EventKind>>, pane_ids: Option<Option<Vec<WireId>>>, fields: Option<Option<Vec<PanelField>>> }`
  The struct is public, with public fields, and the client serializes it. The
  outer `Option` means "key present". A local `deserialize_with` helper wraps
  the inner value in `Some`, so `null` becomes `Some(None)`. This needs no new
  dependency. Serialization uses `skip_serializing_if = "Option::is_none"` on
  the outer `Option`, so `Some(None)` is written as `null`. `events` uses a
  plain `Option`, so a `null` value fails as -32602.
- `crates/noa-ipc/src/push.rs`:
  - `pub enum UpdateSubscriptionError { ConnectionNotFound, SubscriptionNotFound }`.
  - `pub struct SubscriptionUpdate { pub events: Option<EventMask>, pub pane_ids: Option<Option<HashSet<u64>>>, pub fields: Option<Option<FieldMask>> }`.
  - `Broadcaster::update_subscription(&self, conn_id, subscription_id, update) -> Result<(), UpdateSubscriptionError>`.
    It looks the id up only in `conns[conn_id].subs`, so another connection's id
    is `SubscriptionNotFound` by construction. Before releasing the lock it
    records whether the subscription had `OUTPUT` before and after the update.
    After the lock is released it adjusts `output_subscribers` by `+1` or `-1`
    when that changed, the same pattern as `add_subscription` and `remove_subscription`.
- `crates/noa-ipc/src/server.rs`: a dispatch arm `"noa.updateSubscription"`
  calls `require_scope(session, Scope::Read)` and then
  `handle_update_subscription(broadcaster, conn_id, req.params)`. That handler
  parses the params, checks the `MAX_SUBSCRIBE_PANE_IDS` limit, converts the
  params to a `SubscriptionUpdate`, and maps `SubscriptionNotFound` to
  `RpcFail::new(ErrorCode::InvalidParams, "unknown subscription")` and
  `ConnectionNotFound` to -32600, as in `handle_subscribe`.
- `crates/noa-ipc/src/lib.rs`: re-export `PanelField`, `FieldMask`,
  `UpdateSubscriptionParams` and `ChangedPanel`.

### Tests

- `push.rs`: an update replaces `pane_ids` in place, so the same id now matches
  the new pane and not the old one; `Some(None)` clears it; an absent key keeps
  the value; an id from another connection is `SubscriptionNotFound`; an unknown
  id is `SubscriptionNotFound`; changing events `STATE_CHANGED → OUTPUT` flips
  `has_output_subscribers()` and `has_state_changed_subscribers()`; changing
  `OUTPUT → STATE_CHANGED` flips both back; after the update,
  `unregister_connection` leaves `has_output_subscribers()` false, so the
  counter does not drift.
- `server_tests.rs` (needs loopback bind, so it runs with the sandbox off):
  - a round trip returns `{ok:true}`, and a following broadcast respects the new filter;
  - an unknown id returns -32602;
  - an id from a second connection returns -32602, and that connection's
    subscription keeps delivering;
  - 1025 `paneIds` returns -32602 and the filter is unchanged;
  - `"events":null` returns -32602;
  - a client granted no `read` gets -32003.

## AC-4 — Rust client and docs

### Client (`crates/noa-ipc/src/client.rs`)

- `Notification::StateChanged { panels: Vec<Panel>, removed_pane_ids: Vec<u64>, dropped: bool }`.
  `parse_notification` reads `params.removedPaneIds`. When the key is absent
  the list is empty. When the value is malformed, the notification is treated
  like an unreadable `panels` (empty lists, `dropped: true`), so the caller
  lists panels again instead of missing a removal. The reference parser
  `parse_notification_by_ref` in the test module and its differential test
  corpus get the same cases, plus a removal-only frame and a malformed
  `removedPaneIds` frame.
- `Client::subscribe_with(&mut self, events: &[EventKind], panes: Option<&[u64]>, fields: Option<&[PanelField]>) -> Result<u64, ClientError>`.
  `subscribe(events, panes)` delegates with `fields: None`. Its wire request
  stays byte-identical: it sends no `fields` key.
- `Client::update_subscription(&mut self, params: &UpdateSubscriptionParams) -> Result<(), ClientError>`.
- Docs on the `Notification::StateChanged` variant: `dropped` now also means
  that removals may have been lost. The caller lists panels again and treats
  every cached pane missing from that list as removed.

Adding a field to the `StateChanged` variant breaks an exhaustive pattern
(`{ panels, dropped }`) and a struct literal in every crate that builds the
variant or matches it without `..`. These are mechanical edits in the same unit:

- `crates/noa-app/src/ctl.rs`: `apply_notification` (`{ panels, dropped, .. }`)
  and the test literals that build `Notification::StateChanged { .. }`
  (add `removed_pane_ids: Vec::new()`);
- `crates/noa-ipc/tests/orchestration_tests.rs`: the `{ panels, dropped }` pattern.

`mock/agent-dashboard/src/live.rs` matches `{ panels, .. }` and needs no edit.

### Docs

- `docs/specs/noa-server.md`:
  - method table: add `fields?` to `noa.subscribe`, add a row for
    `noa.updateSubscription`, and add `removedPaneIds?, dropped?` to the
    `noa.stateChanged` row;
  - OQ-4: mark it resolved and point to this spec. Note that authorization stays a blanket `read`.
- `docs/feature-map.md`: the "noa-server (JSON-RPC / WebSocket)" row lists
  noa-ipc's code, so add `crates/noa-ipc/src/push.rs` and
  `crates/noa-ipc/src/client.rs` to its Code column. The "Run only these"
  command `cargo test -p noa-ipc` stays as it is.

## AC-5 — Verification

After all units are merged into one branch, run `scripts/verify.sh --full`.
Compare it against the same run on the merge base (`main`), and accept no new
failures. Checks that already fail on the base are listed, not fixed.
`noa-ipc` integration tests and the `noa-pty` tests need the sandbox off
(see `CLAUDE.md`).

## Risks

- **R-1: lock ordering in Broadcaster.** Today's order is `conns` → `PushQueue::inner`
  (`broadcast_*` pushes while it holds `conns`). Connection threads take only
  the queue lock (`drain`). `update_subscription` takes only `conns` and must
  not touch a queue. No new code may take `conns` while it holds a queue lock.
  On the app side, `state_delta` runs under `ipc_shared` (for `registry.resolve`),
  and the guard must be dropped before `broadcast_state_delta`. Today nothing
  takes `ipc_shared` while holding `conns`, so nesting them would not deadlock
  yet. Not nesting them keeps it that way. Both `output_subscribers` updates
  happen after `conns` is released, as they do now.
- **R-2: queue overflow and `removedPaneIds`.** `PushQueue` evicts the oldest
  entry. It never merges entries, and this spec keeps it that way. An evicted
  `stateChanged` can carry the only notice of a removal. The existing contract
  covers that loss: the next drained `output` or `stateChanged` entry carries
  `dropped: true`, and the client must list panels again and diff the result
  against its cache. The client-side buffer (`MAX_BUFFERED_NOTIFICATIONS`)
  follows the same rule. Merging queued `stateChanged` entries to save removals
  was rejected for two reasons. It would reorder `panels` relative to
  `noa.output`. It would also need the queue to understand R-5 union
  semantics. When a burst both changes a pane and removes it, both facts stay
  in arrival order in separate entries.
- **R-3: the `has_state_changed_subscribers` early return in `app/ipc.rs`.** It
  stays, and it stays after the shared listing is replaced. If a pane closes
  while nobody subscribes, the removal is never sent, and none is needed. A
  subscriber that arrives later lists panels first, which is already the
  contract for `+ctl wait` / `send --wait`. Moving the check before
  `mem::replace` would leave a stale `previous` behind, and the first diff
  after someone subscribes would then report false changes and removals.
  `update_subscription` cannot leave the gate wrong: the gate scans `subs`
  live under `conns`, so an update to `events: []` stops counting at once.
  No fields-aware fast path is added to the gate.
- **R-4: the cost of the field diff.** See "Cost of the per-subscription field
  diff". The added cost comes only from changed panels, and the per-subscription
  check is a bit test under a lock the current code already takes.
- **R-5: `Panel` grows later.** `Panel::changed_fields` destructures without `..`,
  and the key-set test in `protocol.rs` compares the `PanelField` names with the
  serialized keys. A new `Panel` field therefore fails the build or that test
  until it gets a `PanelField`. `FieldMask(u32)` has 15 free bits.
- **R-6: older clients.** A removal-only notification reaches a pre-change
  client as `stateChanged` with `panels: []`. `noa-ipc`'s current client parses
  it to an empty update, and so does `+ctl`'s `apply_notification`. A client
  that sends `fields` to an older server has it ignored (FR-19), and so
  receives unfiltered updates. A client that sends `noa.updateSubscription` to
  an older server gets -32601 and can fall back to unsubscribe + subscribe.

## Build units

No two units edit the same file. `ipc-server-core` has to land first: the
other code units compile against its types (`PanelField`, `FieldMask`,
`ChangedPanel`, `UpdateSubscriptionParams`, `broadcast_state_delta`). Its own
edits are not split further, because the new `QueuedNotification` field also
forces the `server.rs` edit, so the edits to `protocol.rs`, `push.rs` and
`server.rs` cannot compile apart.

Order: `ipc-server-core` → (`ipc-client` ∥ `app-state-delta`). `docs` can run
in parallel with any of them. `integrate` comes last.

| Unit | Files (exclusive) | Depends on | Done when |
|---|---|---|---|
| `ipc-server-core` | `crates/noa-ipc/src/protocol.rs`, `crates/noa-ipc/src/push.rs`, `crates/noa-ipc/src/server.rs`, `crates/noa-ipc/src/lib.rs`, `crates/noa-ipc/tests/server_tests.rs` | — | The AC-1/2/3 tests listed for `protocol.rs`, `push.rs` and `server_tests.rs` exist and pass: `cargo test -p noa-ipc --lib` and `cargo test -p noa-ipc --test server_tests` (sandbox off). `cargo clippy -p noa-ipc --all-targets` is clean. `cargo build --workspace --all-targets` succeeds with no other file edited, which proves the compatibility wrappers `add_subscription` and `broadcast_state_changed` hold. |
| `ipc-client` | `crates/noa-ipc/src/client.rs`, `crates/noa-ipc/tests/orchestration_tests.rs`, `crates/noa-app/src/ctl.rs` | `ipc-server-core` | The client parse tests (`removedPaneIds` present, absent, malformed, removal-only, and the updated differential corpus) pass under `cargo test -p noa-ipc --lib client`. An `orchestration_tests.rs` case pins the AC-4 client API end to end: `subscribe_with(.., Some(&[PanelField::Agent]))`, then `broadcast_state_delta` with a removal is read back as `removed_pane_ids == [id]`, and `update_subscription` widens `paneIds` and the next broadcast reaches the new pane (`cargo test -p noa-ipc --test orchestration_tests`, sandbox off). `cargo test -p noa-app ctl::` passes. Clippy is clean for both crates. |
| `app-state-delta` | `crates/noa-app/src/app/ipc.rs` | `ipc-server-core` | `state_delta` replaces `changed_panels`. `sync_ipc_snapshot` broadcasts removal-only deltas and drops the `ipc_shared` guard before it broadcasts. The `diff_tests` cases (changed masks, `ALL` for added panels, removed vs live-unlisted, a pane listed twice) pass under `cargo test -p noa-app diff_tests`. `diff_perf` still compiles (`cargo test -p noa-app diff_perf --no-run`). `cargo clippy -p noa-app --all-targets` reports no new warnings. |
| `docs` | `docs/specs/noa-server.md`, `docs/feature-map.md` | — (follows the shapes fixed here) | The method table, the `stateChanged` row and OQ-4 match this spec. The feature-map row lists `push.rs` and `client.rs`, and every path it names exists (`ls`). |
| `integrate` | no file edits (merge only) | all of the above | `scripts/verify.sh --full` on the merged branch shows no failure that is absent from the same run on `main` (AC-5). |
