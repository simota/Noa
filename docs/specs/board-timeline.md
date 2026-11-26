# Board Timeline: Spec

| | |
|---|---|
| slug | board-timeline |
| status | design locked for the factory run (2026-11-26); not built |
| owner | simota |
| as-of | 2026-11-26, HEAD 3d6ed594 (the proposal was surveyed at 765da968; nothing it cites changed in between) |
| crates | `crates/noa-ipc`, `crates/noa-config`, `crates/noa-app` (server, `+ctl`), `widgets/{tasks,review,verify,discussion,competition}` |
| request | the human proposal "Noa Board Timeline 設計書" (source of truth for behaviour; cited as §N) and acceptance criteria AC-1 … AC-8 of this run |

This is scaffolding. What outlives the build are the Decisions and the wire contract (L2-2), which move into `docs/specs/noa-server.md` once units land. Statements about today's code were read at HEAD with the file and line given. **[unverified]** marks what this spec could not check.

## L0: Vision

- Problem: what a worker achieved lives in its `$NOA_RESULT` file, in a widget's event log, or in the board's in-memory report history (50 reports per worker, 5 closed workers). Close the pane, the board or Noa and the history is gone; another agent cannot read it at all.
- Job: one durable, append-only feed per board of what workers did (summary, outcome, checks, open issues, artifacts), readable by a person in `+ctl board` and by agents through `+ctl timeline` / JSON-RPC, written explicitly by the worker or by a widget runner at a confirmed event. Posting never starts work (§1, §3).
- Out of scope (MVP, §3, §9, §13): push notifications, LLM summaries, comments/reactions, strict read state, cloud/multi-user sync, a GUI viewer, ACLs.

## Verification of the proposal's claims

Each claim §2/§8/§9/§10/§11 makes about today's code, checked at HEAD.

| # | Claim | Verdict | Evidence / adjustment |
|---|---|---|---|
| V1 | board is an ANSI TUI with `List` and `Detail` | true | `enum View { List, Detail(u64) }` `crates/noa-app/src/ctl/board.rs:359`; `Action { Focus, Send, Close, Quit }` :381; pure `render(cols, rows, now) -> Vec<String>` :2454; only `Screen` (:4211) writes to the tty |
| V2 | history is `Panel.agent.seq`, 50/worker, closed workers 5 | true, with a nuance | `HISTORY_CAP = 50` :37 (reports per worker), `GONE_CAP = 5` :53 is **5 closed workers**, not 5 reports of a closed worker; `Board::update` :692-868 |
| V3 | refresh: list 500 ms, screen 1 s, task/result files 2 s | true | `LIST_EVERY` :23, `SCREEN_EVERY` :35, `insight::FILES_EVERY` `board/insight.rs:22` |
| V4 | `insight.rs` lazily reads results | true | it reads the parent's ctl journal and each worker's result file, by `(mtime,len)` stamp, `O_NONBLOCK`, regular files only (`read_whole` :269) |
| V5 | key/width conventions in board.rs | true; keys `t / f G w` are free | bound today: `q`/Ctrl-C quit-confirm, `?`, `n`, Space, digits, `j/k`, `r`, Enter/`l`/Right, Esc/`h`/Left, `g` focus, `s` send line, `x` close-confirm (`key_at` :1529-1701). Width: `WIDE_COLS = 110`, `STACKED_COLS = 70` with `STACKED_ROWS = 18` (:43-46), `MIN_COLS/MIN_ROWS = 24/6` (:72-73). Width via `unicode_width` (:13), sanitiser `clean()` :2423 |
| V6 | the board guards against another Noa instance | true | `same_instance` :4361 compares `HelloResult.server_instance_id` on reconnect; `hold_elsewhere` :4619 blocks actions. Pane ids are not cross-checked |
| V7 | ctl journal rotates at 4 MiB | true | `MAX_JOURNAL_BYTES = 4 MiB` `ctl/journal.rs:28`, drops the oldest half (`drop_oldest_half` :181). Not usable as the timeline's store — adopted |
| V8 | `Notification::Other` keeps no payload | true | `client.rs:56-72`, `parse_notification` keeps only `method`, `dropped` (:1126). Irrelevant for the MVP (no push) |
| V9 | scopes read/control/input in `auth.rs` | true (+`attach`) | `enum Scope { Read, Control, Input, Attach }` `auth.rs:10-15`. There is **no method→scope table**: each `dispatch` arm calls `require_scope` (`server.rs:1534-1591`) |
| V10 | "reuse the existing invalid-params / scope / method codes, add new codes; failures carry `data.kind`" | partly false | codes exist (-32602, -32003, -32601; -32001…-32011 used, `error.rs:33-55`), but **no error carries `data` today**: `error_response` writes `{code,message}` only (`server.rs:1417`), and the client's `RpcError {code,message}` (`client.rs:1185`) drops `data`. Adjusted: server adds `data`, the client derives the kind from the code (D7) |
| V11 | capability via `noa.hello` | true, but static | `SERVER_CAPABILITIES: &[&str] = &[CAPABILITY_BOARD_TAB]` `protocol.rs:821`; `handle_hello` runs in noa-ipc without the backend (`server.rs:1614`). Adjusted: the backend contributes capabilities (D5) |
| V12 | `rand`, `serde_json`, `libc`/file lock in `crates/noa-app/Cargo.toml` | true | noa-app: `rand`, `serde_json`, `libc`, `dirs`, `parking_lot`, `crossbeam-channel`. noa-ipc: `rand`, `serde_json`, `serde`, `crossbeam-channel`, `parking_lot`, `libc` (unix only); no `dirs`, no `noa-config`. No lock crate anywhere: flock via `libc::flock` (`widgets/tasks/src/runs.rs:64`) or std `File::try_lock` (`widgets/review/src/store.rs:348`) |
| V13 | review store has JSONL/replay/lock; tasks uses `sync_data` | true | review: torn tail truncated, complete bad line → `StoreError::Corrupt` (:370), lock held for the whole session, **append does not fsync**. tasks: `runs::append` ends in `sync_data()` (`runs.rs:115`) |
| V14 | `widgets/{discussion,review,verify,competition}/src/model/export.rs` are the "existing result models" | partly false | the four files exist but are **Markdown renderers** (`&Session -> String`), not models. The structured confirmed results are the event payloads: `ModeratorOutput` (`discussion/src/model/types.rs:766`), `LeadOutput`/`Verdict` (`review/.../types.rs:1036/979`), `JudgeOutput`/`Ruling`/`Verdict` (`verify/.../types.rs:1729/1670/1719`), competition `JudgeOutput`/`EntryEvaluation` (`competition/.../types.rs:436/348`). Adjusted: adapters map those types; export.rs output is only used as `bodyMarkdown` |
| V15 | competition's blind vs mutual view is in `export.rs` | partly false | the switch is `Session.reveal_interval` (`competition/src/model/types.rs:664`, `RoundKind::for_round` :519; 0 = fully blind); export.rs only prints its label (:24-28) |
| V16 | widgets have confirmed events to publish from | true except tasks | event enums: `discussion/.../reduce.rs:16`, `review/.../reduce.rs:18`, `verify/.../types.rs:2211`, `competition/.../reduce.rs:18`. **Events carry no id**; lines are batches. Tasks has no "result accepted" event (only `RunStarted/RunPaneAssigned/RunClosed`, `tasks/src/model/types.rs:203`) — its result is the `result.md` file. Adjusted: deterministic event ids (L2-7), tasks publishes on an explicit share |
| V17 | widgets can call "a common publisher" | true, by path dep | every widget is its own workspace with its own `Cargo.lock` (empty `[workspace]`); the only path deps are `noa-ipc` and `noa-config`; no shared widget crate (sharing is by byte-identical copies, `docs/specs/task-links.md` Reuse decision). The publisher therefore lives in **noa-ipc** (D3). `rand 0.8.7` is already in every widget lock via noa-ipc, so no lock changes |
| V18 | D20 board tabs in `docs/specs/agent-orchestration.md` | true | `agent-orchestration.md:431`; `noa.newTab {board:true}` (`protocol.rs:807`), `Client::spawn_board_tab` (`client.rs:279`), workers are floats with `parentPaneId = board` |
| V19 | spawn/send/retry and prompt metadata | partly | `spawn --task`, `send`, `retry` exist (`ctl.rs` `parse_spawn` :642, `parse_send` :788, `parse_retry` :769; `Session::spawn` :2150, `deliver` :2499, `Session::retry` :2418). **No prompt metadata exists** except retry's preface (`ctl/retry.rs:21-23`, `strip_preface` :226). No `workerId`, `boardId` or root-orchestrator id exists anywhere; a worker is `paneId` + `parentPaneId` + `label` + `surfaceId` (`protocol.rs:175-212`) |
| V20 | data-dir helpers | no generic helper | `noa-config/src/lib.rs`: `session_state_path` :2247, `ctl_journal_path(_in)` :2258/:2265, `scrollback_dir` :2286 — each joins `dirs::data_dir()/noa`. New paths follow the `foo()` + `foo_in(&Path)` pair |
| V21 | the server can do work off the UI thread | true | thread per connection, sync tungstenite (`server.rs:373/431`, `noa-ipc/src/lib.rs:14-16`); `noa.listPanels` and `noa.getText` are answered from `IpcShared` without the UI thread (`ipc_bridge.rs:427-451`); UI-thread actions go through `submit` with a 2 s `REQUEST_TIMEOUT` → `-32009 OutcomeUnknown` (`ipc_bridge.rs:300,401-423`) |
| V22 | pane ids are stable enough for live links | true | IPC pane ids are minted monotonic per process (`IpcRegistry::mint`, `ipc_bridge.rs:42-60`) and kept across a pane move (`rekey_pane` :145); `serverInstanceId` is 16 random bytes per process (`server.rs:1604`) |
| V23 | `crates/noa-app/src/atomic_write.rs` suffices for metadata | partly | it does staging + `sync_all` + rename, but **no directory fsync** (§8.1 requires it). Adjusted: the store fsyncs the directory after rename and after first creation |

## Decisions

- **D1 — one service thread.** `TimelineService` is one `std::thread` in the GUI process owning every board: sequential processing gives the in-process single writer for free; cross-process exclusion is a non-blocking `flock` per board (§8.1). Requests come over a bounded `crossbeam_channel` (capacity 64); full → `busy`. The UI thread and the io/PTY thread never touch it.
- **D2 — dispatch path.** Connection thread → `dispatch` arm `noa.timeline.*` → `IpcBackend::timeline(call)` → `AppIpcBackend` clones `Arc<Vec<Panel>>` from `IpcShared` (already off the UI thread, V21) and sends `Job { call, panels }` to the service; waits up to 4 s (under the client's 5 s `CONTROL_REQUEST_TIMEOUT`). Timeout on a write → `outcome_unknown` (code -32009, `data.requestId`); on a read → `busy`.
- **D3 — publisher in noa-ipc.** Wire types, the client wrappers and the outbox/publisher live in `crates/noa-ipc` (`timeline.rs`, `timeline_outbox.rs`), the only workspace crate every widget and `+ctl` already depend on by path. noa-ipc gets no new dependency (it keeps not depending on `dirs`/`noa-config`: callers pass the outbox directory, resolved with the new `noa-config` helpers).
- **D4 — store in noa-app.** `crates/noa-app/src/timeline/` holds store, ids, replay and the service (§11). Widgets never open `posts.jsonl` (§11).
- **D5 — capability is dynamic.** `boardTimelineV1` is advertised only when the service started (data dir resolvable). `IpcBackend::capabilities()` (default empty) is merged into `HelloResult.capabilities`.
- **D6 — UI thread does nothing.** IDs are minted and live bindings recorded by the service from the panels snapshot. (§7.1 has the UI thread reserve IDs; not needed, and keeping the UI thread out is the safer reading of §8.)
- **D7 — errors.** New codes -32012…-32018 (L2-3) plus `error.data = {kind, …}` on the wire. The Rust client keeps `ClientError::Rpc {code, message}` unchanged (it is matched by shape in `+ctl` and five widgets) and offers `TimelineErrorKind::from_code`.
- **D8 — run registration happens in `+ctl`/the runner, not in `noa.split`.** `+ctl spawn --task`, `+ctl send --new-run` and `+ctl retry` call `noa.timeline.registerRun` after the pane exists and before typing the task; the run id reaches the worker in a one-line prompt header (L2-6). No env var carries a run id (§7.1).
- **D9 — board resolution by orchestrator pane.** `registerRun` with `orchestratorPaneId = P`: P is a bound worker → its board (child inheritance, matching the server's one-level nesting D13); P is a bound root → its board; otherwise a new board bound to root P. A widget passes its board-tab pane and, after a restart, the `boardId` it saved (§5).
- **D10 — publish responsibility change is an explicit call.** An 8th method `noa.timeline.setPublishPolicy` (control) records an owner change (`worker` → `manual`) so an orchestrator may import a result-only worker's `$NOA_RESULT` (§5, §7.1 "明示的なcontrol操作として記録"). Recorded in run metadata with its history.
- **D11 — polling only.** Board TUI polls `changes` every 2 s; no notification type is added (§9).
- **D12 — search is a scan.** No index file in the MVP: summaries are in memory, bodies are read by offset. AC-6 measures it.

## Glossary

| Term | Meaning | Identifier |
|---|---|---|
| board | durable sharing unit, independent of pane/tab/repo | `bd-<16 hex>` |
| root | the pane a board was created for (an orchestrator, or a widget's board tab) | live binding `{serverInstanceId, paneId, surfaceId}` |
| worker | one spawned pane on a board; never reused | `wk-<16 hex>` |
| run | one attempt at one assigned piece of work | `rn-<16 hex>` |
| post | one immutable timeline entry | `po-<16 hex>`, `seq` per board |
| producer | who sent the post (worker id, `<ns>:<session>` runner, or a CLI/human id) | `producerId` string |
| live link | a worker binding whose `serverInstanceId`, `paneId` and `surfaceId` all match the current listing | `ContextWorker.live` |
| outbox | client-side envelopes persisted before sending | `<data>/noa/timeline-outbox/…` |

## L1: Requirements (per AC)

### AC-1 Store & API

Approach: L2-2 wire types, L2-4 store, L2-5 service. Methods (all JSON-RPC, object params, camelCase):

| Method | Scope | Params → Result |
|---|---|---|
| `noa.timeline.context` | read | `ContextParams` → `ContextResult` |
| `noa.timeline.boards` | read | `BoardsParams` → `BoardsResult` |
| `noa.timeline.registerRun` | control | `RegisterRunParams` → `RegisterRunResult` |
| `noa.timeline.setPublishPolicy` | control | `SetPublishPolicyParams` → `RunInfo` (D10) |
| `noa.timeline.post` | control | `PostParams` → `PostResult` |
| `noa.timeline.list` | read | `ListParams` → `PageResult` (desc) |
| `noa.timeline.get` | read | `GetParams` → `GetResult` |
| `noa.timeline.changes` | read | `ChangesParams` → `PageResult` (asc) |

Rules fixed for tests:

1. `seq`, `beforeSeq`, `afterSeq`, `headSeq`, `nextBeforeSeq`, `nextAfterSeq` are decimal strings on the wire (`Seq(u64)`, serialised like `WireId`, `protocol.rs:32`); a JSON number there is refused (`invalid_params`), since a number above 2^53 loses precision in a JS client. Pane ids stay `WireId`. `params` must be a JSON object (absent or `null` reads as `{}`); an array or scalar is `invalid_params`.
2. Limits: title 1..=120 chars, summary 1..=400 chars (Unicode scalar values; blank = only `char::is_whitespace` → rejected), `bodyMarkdown` ≤ 32 KiB UTF-8 bytes, `openIssues`, `nextActions`, `checks`, `artifacts`, `basedOnPostIds` ≤ 100 items each (`MAX_LIST_ITEMS`), an artifact's `label` ≤ 128 bytes, `occurredAt` a real calendar date (month lengths, leap years), the raw `params` JSON of `post` ≤ 64 KiB, every response ≤ 192 KiB, board file ≤ 100 MiB. Over any limit → `invalid_params` (or `storage_full`), never truncated. The item caps keep the normalised post inside a `get` response: normalising adds at most 20 bytes per item (`"x"` → `{"ref":null,"text":"x"}`; an artifact gains `"digest":null`, a check `"evidenceRef":null`) and under 1 KiB of spelled-out absent fields, so a post from 64 KiB of params normalises to under 75 KiB, and the stored `Post` with its server fields stays far below 192 KiB.
3. `list`: `seq < beforeSeq` (absent = from head), descending, `limit` default 20, max 50, no bodies; stops early when the next item would push the encoded result over 192 KiB; `hasMore`, `nextBeforeSeq` = last returned seq. `changes`: `seq > afterSeq` ascending, `limit` default 100, max 200, same byte rule; `nextAfterSeq` = last returned seq (or `afterSeq` if none). Both return `headSeq` captured at the start of the call, and only posts with `seq ≤ headSeq`, so a page never contains a post that arrived during it. `boards` (archives included; boards are never deleted): newest activity first by `board_listing_order`: `activityAt` (`BoardInfo::activity_at` = the later, as instants, of the board's `createdAt` and `lastActivityAt` (the greatest `createdAt` among the board's posts), or the board's `createdAt` while it has none; the max keeps the key from moving backwards when a post records a `createdAt` earlier than the board's own after a clock correction) descending, compared as instants (`…:05Z` = `…:05.000Z` < `…:05.5Z`), equal ones by `boardId` ascending, so the order is total. `before` (a `BoardCursor {activityAt, boardId}`; absent = from the newest) returns only the boards after that position (`BoardCursor::admits`); it is a position, so the board it names need not exist; a malformed `boardId` or an `activityAt` that is not RFC 3339 UTC `YYYY-MM-DDTHH:MM:SS[.fff]Z` → `invalid_params`. `limit` default 50, max 200; `hasMore` = a board follows the last returned one; `nextBefore` = the last returned board's cursor (`null` when none is returned). Paging promise: a board's `activityAt` never decreases and positions strictly descend along a pass, so a pass returns no board twice, and returns every board whose `activityAt` is unchanged during it exactly once. A board whose `activityAt` grows during a pass may be skipped by that pass (a fresh pass from the newest lists it first) or appear once later in it (when its new position still falls after the cursor, e.g. an equal instant and a larger `boardId`).
4. Idempotency (§6.1): key A = `(boardId, producerId, requestId)`; key B = `(boardId, sourceRef.namespace, sourceRef.sessionId, sourceRef.runId, sourceRef.eventId)`. Canonical payload = `serde_json::to_vec` of the normalised `PostCanon { runId, producerId, post: PostInput }` (struct field order is fixed, so client key order is irrelevant; absent arrays → `[]`, absent options → `null`, a string item → `{text, ref: null}`). A hit with equal canon → the stored `{postId, seq, createdAt}` with `duplicate: true`; unequal → `conflict` (`data.existingPostId`) and nothing written. Key B uses the canon without `requestId` and without `producerId` (a source event is one event whoever relays it, §6.1). A key-B duplicate uses up its own key A: the store records `(producerId, requestId) → postId` (`meta.json` `requestAliases`) before answering, so that request id sent again with the same payload is that post again and with another payload `conflict`, also after a restart. Before any duplicate is acknowledged its line is made durable (`sync_data`, unless this store already saw it synced): a line replayed after a restart may be one whose sync failed (§8.1 "再送で既存行を見つけても、永続化を再確認してからackする"); a failed sync answers `io_error` and poisons the board.
5. One initial summary per registered run: `kind = summary` with `supersedesPostId = null` a second time on the same run → `conflict`. A correction requires `supersedesPostId` naming a post of the same board and a new `eventId`: the same `sourceRef.eventId` as the post it supersedes → `invalid_params`, whatever else of its `sourceRef` differs.
6. A post on a registered run must come from that run's `publishPolicy.producerId` (else `invalid_params`, "producer not registered for run"). `runId = null` is allowed only with `sourceRef.namespace = "result-file"` or `"manual"`; such posts are stored with `provenance: "unverified"` (shown 「出所未照合」).
7. `retryOfRunId`, `basedOnPostIds`, `supersedesPostId` must exist in the same board (`invalid_params`).
8. Files: `<data>/noa/boards/` and each board dir `0700`; `meta.json`, `posts.jsonl`, `lock`, outbox files `0600` (created with `OpenOptionsExt::mode`, then `fchmod` to defeat umask). `boardId` must match `^bd-[0-9a-f]{16}$` before any path is built; a non-matching id → `invalid_params`.
9. Store I/O and search run only on the service thread (a test asserts the handling thread's name is `noa-timeline`).
10. `context` stays inside the 192 KiB response: when every run would not fit, the oldest runs are left out first (the calling pane's own two most recent are always kept, so `self.runs` still tells one run from several), then the workers that are not live, oldest first. Each `runs` list is the most recent runs of its worker, in registration order; the contract has no "truncated" flag, so a caller that needs an older run reads it from the post that names it.
11. A board whose directory (its `meta.json`) is removed by hand while Noa runs — the `storage_full` advice — is dropped from the store on its next read: it leaves `boards`, and every read or write of it answers `not_found`. `boards` leaves out (with a logged warning) a loaded board it cannot read, e.g. one whose `meta.json` no longer parses, instead of failing the whole listing, as the scan leaves out a board it cannot load.

Tests (`crates/noa-app/src/timeline/tests.rs`, store driven directly with a temp root, plus service tests through `TimelineHandle`):
`ac1_replay_after_restart_keeps_seq_order`, `ac1_same_key_same_payload_returns_same_post`, `ac1_key_order_and_defaults_do_not_matter`, `ac1_same_key_other_payload_conflicts_and_store_unchanged`, `ac1_same_source_other_request_collapses`, `ac1_same_source_other_body_conflicts`, `ac1_second_initial_summary_conflicts`, `ac1_concurrent_producers_lose_nothing` (8 threads × 50 posts through the handle; seq 1..=400 contiguous, all ids distinct), `ac1_torn_tail_quarantined_and_resend_succeeds`, `ac1_corrupt_middle_line_stops_writes` (reads still work; writes → `corrupt_store` with `data.line`), `ac1_storage_full_rejects_without_writing` (limit injected via `StoreLimits`), `ac1_sync_failure_rebuilds_before_next_seq` (fault injection hook `StoreIo` trait), `ac1_limits_rejected_not_truncated` (title 121, summary 401, body 32 KiB+1, blank title, params 64 KiB+1), `ac1_list_and_changes_pages_have_no_gaps_when_posts_arrive_between_pages`, `ac1_list_byte_cap_sets_has_more`, `ac1_boards_pages_reach_every_board_once` (201+ boards through `before`/`nextBefore` at `limit` 200; a post on a board between pages neither repeats it nor drops another), `ac1_last_activity_never_decreases` (a replayed `posts.jsonl` whose later line has an earlier `createdAt` keeps `lastActivityAt` at the greatest), `ac1_modes_0700_0600`, `ac1_bad_board_id_rejected_before_path`, `ac1_runs_on_service_thread`, `ac1_second_instance_lock_is_busy` (second `Store` on the same root → `busy` for writes), `ac1_resend_after_failed_sync_is_synced_before_ack`, `ac1_failed_read_during_rebuild_keeps_the_board_poisoned`, `ac1_failed_catch_up_after_lock_drops_the_writer`, `ac1_mistyped_middle_line_stops_replay`, `ac1_store_dirs_are_made_durable_on_the_service_thread`, `ac1_store_that_cannot_open_is_not_opened`, `ac1_key_b_duplicate_uses_up_its_request_id`, `ac1_correction_needs_a_new_event_id`, `ac1_query_matches_body_by_unicode_lowercase`. Wire tests in `crates/noa-ipc/src/timeline.rs` (`seq_is_decimal_string`, canon golden bytes, `TimelineCall::parse` per method, the `boards` cursor and order) and `crates/noa-ipc/tests/server_tests.rs` (fake backend: scope denials per method, `data.kind` present, capability only when the backend reports it, `-32601` for unknown `noa.timeline.*`).

### AC-2 CLI

Approach: `crates/noa-app/src/ctl/timeline.rs` (L2-8). Subcommands: `context`, `post`, `list`, `show`, `changes`, `export`, `boards`. `--board` defaults to the board of the calling pane (`context` with own pane from `NOA_SURFACE_ID`, `ctl.rs` `own_pane` :1619). Output: one JSON document on stdout (`print_json`, `ctl.rs:4363`). Exit: 0 durable ack/success; 1 error; 3 when the server lacks `boardTimelineV1` (message names the capability and, inside a worker, its `$NOA_RESULT` path, §9 last lines). A request Noa did not confirm (`outcome_unknown` -32009, or no reply within the client's deadline) is 1, not 3: for a write (`post`, `--take-over`'s `setPublishPolicy`) the message names the requestId it was sent under, which re-running the same command resends; a read says it is safe to run again. The reads and `setPublishPolicy` keep the exit 3 every `+ctl` command gives a lost connection, a server at its connection limit and a token without the scope (`ctl.rs` `impl From<ClientError> for Failure`); the post send itself reports every failure as 1, since its envelope stays staged and a re-run resends it.

- `post --file F`: JSON `PostInput`, read as `--from-result` reads its file (regular file only, no FIFO wait) up to `MAX_POST_PARAMS_BYTES` (64 KiB; over → refuse); `boardId/runId/producerId/requestId` come from flags/context only, and a file that also contains any of them is refused (§9 "競合する二重指定を拒否"). With a run and no `--producer`, the producer is the run's (its publish policy's `producerId`, or its worker); one that cannot be found is refused ("pass --producer ID"), for `--file` and for `--from-result --run R` alike; `cli:manual` is used only without a run or with `--take-over`. `--request-id` absent → `cli-<fnv1a64 of (boardId, runId, producerId, canonical PostInput)>` so re-running the same command is idempotent.
- `post --from-result PATH --title T --summary S --outcome O [--kind K] [--take-over]`: reads PATH with `open(O_RDONLY|O_NOFOLLOW|O_NONBLOCK|O_CLOEXEC)`, `fstat` → must be `S_ISREG`, read ≤ 32 KiB + 1 (over → refuse); `sourceRef = {namespace: "result-file", sessionId: <runId or "none">, eventId: "file-<fnv64(path)>-<fnv64(bytes)>"}` and an artifact `{kind: file, ref: PATH}`. `--take-over` stages the envelope in the outbox first, then calls `setPublishPolicy(owner = manual)` for that run (D10) under requestId `cli-policy-<fnv1a64 of (boardId, runId, producerId, the post's requestId)>`, so re-running the same command replays the same policy change while a later take-over (another post, even by a producer that held the run before) records a new one, then sends, so a post the outbox refuses (local conflict, write error) leaves the policy unchanged.
- Outbox: before the first send the envelope is written to `<data>/noa/timeline-outbox/cli/<key>.json` (0600, staging + fsync + rename + dir fsync); it is removed only after a durable ack (`PostResult`). An existing envelope with the same key and different content → refuse (local conflict) without sending. Outbox write failure → exit 1, nothing sent (§7.2).
- `boards [--before ACTIVITY_AT,BOARD_ID] [--limit N]`: prints `BoardsResult`; the next page is `--before` with the printed `nextBefore`'s `activityAt` and `boardId` joined by a comma (`BoardsParams.before`).
- `export --format markdown|json --output PATH`: pages `changes(afterSeq=0)` to the head, `get`s every post, renders, then `atomic_write::write_atomic`. Each exported post keeps `boardId`, `postId`, `seq`, `sourceRef`. No referenced file contents.

Tests (inline in `ctl/timeline.rs`): `ac2_fifo_refused_without_hanging` (mkfifo in a temp dir; the call returns within 1 s), `ac2_symlink_refused`, `ac2_over_cap_refused`, `ac2_file_with_ids_refused`, `ac2_default_request_id_is_stable`, `ac2_outbox_written_before_send_and_removed_after_ack` (fake `TimelineApi` trait object that fails, then acks), `ac2_no_ack_exits_nonzero_and_keeps_envelope`, `ac2_export_markdown_and_json_keep_ids` (golden), `ac2_export_is_atomic` (no partial file on a fetch failure), `ac2_parse_*` per subcommand, `ac2_unconfirmed_reads_exit_1_and_only_the_capability_is_exit_3`, `ac2_unconfirmed_writes_exit_1_and_name_the_request_id`, `ac2_take_over_refused_locally_leaves_policy_unchanged`, `ac2_post_file_is_read_bounded`, `ac2_from_result_with_unknown_producer_is_refused`, `ac2_boards_pages_through_before`.

### AC-3 Board view

Approach: pure model + render in `crates/noa-app/src/ctl/board/timeline.rs` (L2-9); routing in `board.rs`. `View` gains `Timeline` and `PostDetail(PostId)`; `t` toggles `List|Detail` ↔ `Timeline|PostDetail`.

- Keys in Timeline/PostDetail: `j/k`/Down/Up select (scroll body in PostDetail), Enter open, Esc back (or close the input line), `/` query line (title+summary+body, sent as `list.query`), `f` filter line (Tab cycles worker/agent/kind/outcome, Left/Right cycles the value, Enter applies, Esc cancels), `G` merge pending new posts and select the newest, `g` focus only when the post's worker is live (Glossary), `w` switch to the worker's `Detail(pane)` when that pane is in the board's listing, else footer "worker is not on this board". `s`/`x` do nothing there except a footer hint "w でワーカーへ". `q`/Ctrl-C keep today's quit path (`key_at` :1618).
- Layout: `cols ≥ 110` list + preview at `cols*2/5` on the right; `cols ≥ 70 && rows ≥ 18` preview below (height `(room/3).clamp(3,8)`); else list only, Enter → full PostDetail. Below 24×6 today's "Widen to 24×6" note (`render` :2456).
- List item: line 1 time (local, `crate::localtime`), author label, agent kind, role, kind, outcome; line 2 title; up to 3 summary lines; aux line `task/run`, checks `passed/failed/not_run`, open issues, artifacts, `訂正あり`/`訂正` marks, `結果ファイルから取り込み` when `sourceRef.namespace == "result-file"`, `出所未照合` when `provenance == unverified`. Outcome and verification shown separately (`検証未実施` when no check or all `not_run`).
- New posts: `changes(afterSeq = receive cursor)` every 2 s (≤ 5 pages per tick). At the latest position (selected index 0, list not scrolled) they merge at once; otherwise they wait in `pending` and the header shows `新着 N (G)`; selection and scroll stay put.
- "前回閲覧以降 N 件": `visit boundary` = the highest seq shown when the timeline was last left, stored per board in `<data>/noa/timeline-visits.json` (0600, atomic). N = posts with seq > boundary; it is not decremented by agents (an agent's `afterSeq` is its own). The boundary is written when the person leaves the timeline view or quits.
- Disconnect: a failed `changes`/`list` keeps the last posts and shows `更新停止 HH:MM:SS` (last success).
- All post text passes `clean()` (control characters removed; ESC included) before width layout; widths via `unicode_width`.
- `Noa +ctl board --timeline --board-id B` opens B in Timeline; with no live root the worker list says `アーカイブ: ライブworkerなし` and `g`/`w` are inert.
- Server without `boardTimelineV1`: `t` shows `タイムライン未対応のNoa` once; everything else unchanged.

Tests (inline, the existing style `board.key(Key::…)` / `rendered()`): `ac3_t_toggles_views`, `ac3_enter_esc_detail`, `ac3_slash_sets_query`, `ac3_f_cycles_filter`, `ac3_new_posts_merge_only_at_latest`, `ac3_pinned_selection_while_reading_old`, `ac3_capital_g_merges_and_selects_newest`, `ac3_g_only_for_live_worker` (instance mismatch, surface mismatch, pane gone → no `Action::Focus`), `ac3_w_jumps_to_worker_detail`, `ac3_s_x_inert_in_timeline`, `ac3_layout_110_70_and_24x6` (`assert_widths` :8168), `ac3_since_last_visit_count`, `ac3_stale_banner_after_failure`, `ac3_control_chars_removed`, `ac3_japanese_width`, `ac3_archive_mode`, `ac3_unsupported_server_message`. The look in a real window is AC-8 (manual).

### AC-4 Worker run attribution

Approach: `crates/noa-app/src/ctl/run_attr.rs` (L2-6) called from `ctl.rs` hooks.

- `spawn --task` (and `--task-file`): after the pane is ready (`ready::wait_ready`) and before `deliver`, `run_attr::register(.., NewRun::SpawnTask)`; the typed text becomes `header + task`.
- `send --new-run` (new flag): first runs every check `send` makes before typing (text size with the header counted, unknown pane, an agent waiting for an approval or answer, a working agent, bracketed paste not yet on), so a send that would be refused registers nothing; then registers a new run for the target pane and sends `header + text`. Plain `send`, `send --answer`, `--queue`, `--replace` never register (same run, §7.1); `--new-run` together with `--answer`, `--raw`, `--queue` or `--replace` is refused when the arguments are parsed.
- `retry`: the new worker's run gets `retryOfRunId` = the old worker's latest run id from the journal (`Worker.run_id`, new field), or from `context(old pane)` if the journal lacks it.
- `requestId` for registration: `ctl-<16 hex rand>` minted once per command, resent unchanged on `busy`/`outcome_unknown` (up to 5 attempts, 200 ms → 1.6 s backoff).
- Board inheritance by D9; a worker that spawns its own workers passes its own pane as orchestrator, so its children share its board.
- Journal: every registered run (`spawn --task`, `retry`, `send --new-run`) appends its own `{"ev":"run","pane","boardId","runId","workerId"}` event; the `spawn` event itself is unchanged (it is journaled before the run is registered). `spawn`'s stdout line carries `boardId`, `runId`, `workerId`. U6 folds the `run` events into `Worker.run_id` (a pane's latest run).
- `timeline post` without `--run`: `context(own pane).self.runs` — exactly one → used; zero → refuse "no registered run (pass --run, or --from-result with --take-over from the orchestrator)"; more than one → refuse listing the run ids (§7.1). With `--board` and a `sourceRef.namespace` that allows `runId = null` (`manual`, `result-file`), a calling pane on no board (its `context` answers `not_found`) or on another board posts with no run, as the same command outside a pane does.
- Live links only when `serverInstanceId` (hello) equals the binding's, the pane is in the current listing, and its `surfaceId` equals the binding's (§5; surfaceId is the extra guard against a reused number in another instance).
- Older server: `supports_board_timeline()` false → no registration, no header, spawn/send/retry behave exactly as today; `timeline *` exits 3 with "unsupported".
- Registration failure after retries: the task is still delivered without a header and `+ctl` prints `timeline: run not registered (<kind>): this worker cannot publish` on stderr (open question Q1).

Tests: `run_attr` pure (`header_round_trip`, `strip_header_then_preface`, `choose_run_zero_one_many`, `request_id_stable_across_retries` with a fake `TimelineApi` returning `outcome_unknown` then success); store tests `ac4_register_run_idempotent`, `ac4_register_run_other_payload_conflicts`, `ac4_child_inherits_parent_board`, `ac4_root_rebind_after_restart_with_board_id`, `ac4_retry_links_retry_of`, `ac4_live_only_when_instance_pane_surface_match`, `ac4_root_rebind_keeps_runs_another_instance_registered`, `ac4_register_run_resend_finds_another_instances_run`, `ac4_register_run_resend_after_panes_closed`, `ac4_run_order_is_registration_order`; ctl tests `ac4_spawn_task_registers_before_typing` (fake server records call order: `registerRun` before `sendText`), `ac4_answer_and_follow_up_do_not_register`, `ac4_send_new_run_registers`, `ac4_old_server_unchanged` (no capability → byte-identical typed text), `ac4_post_without_run_ambiguous_refused`; insight `ac4_task_text_strips_header`.

### AC-5 Widget adapters

Approach: one shared publisher (`noa_ipc::timeline_outbox`, L2-7); each widget adds `src/timeline.rs` with a pure `candidates(&Session) -> Vec<Candidate>` from its event/output types (never UI strings) and runner glue.

| Widget | Run registration | Confirmed point → `eventId` | Post |
|---|---|---|---|
| tasks | at `RunStarted` + pane assigned: `orchestratorPaneId = board`, `workerPaneId = run pane`, owner `runner`, producer `tasks:<repo-key>`, `taskRef {kind: local|github, id}` | the person presses 共有 on a run's result → `result-<run-id>` | kind summary, outcome `unknown` unless the person picks one in the share form, body = `result.md` text (over 32 KiB: first 32 KiB cut at a char boundary + "（全文は result.md）", artifact ref to the file — the client excerpts explicitly, the server never truncates) |
| review | session open: owner `runner`, producer `review:<session>`, no worker pane | `SessionClosed` after `LeadAccepted` with `verdict: Some` → `verdict-r<round>` (later rounds supersede the previous) | summary = `LeadOutput.summary`; checks: one per verdict (`Approve`/`ApproveWithFixes` → passed, `RequestChanges`/`Reject` → failed); openIssues = open/agreed findings titles; body = `export::report_markdown` |
| verify | same, producer `verify:<session>` | `SessionClosed` after `JudgeAccepted` with `verdict: Some` → `verdict-r<round>` | verdict check (`Pass`/`PassWithIssues` → passed, `Fail` → failed, `Inconclusive` → inconclusive), followed by checks from `rulings` (`Verified` passed, `Failed` failed, `Inconclusive` inconclusive, `NotApplicable` not_run); openIssues = `Verdict.blocking`; body = `export::report_markdown` |
| discussion | same, producer `discussion:<session>` | `SessionClosed` → `closed-<n>` (uses the last `ModeratorOutput` at that close) | kind decision; summary from `conclusion` (or `summary`); openIssues = minutes `OpenQuestions`, nextActions = `Actions`; body = `export::result_markdown` |
| competition | same, producer `competition:<session>` | `SessionClosed` only → `closed-<n>` | kind summary; checks per entry score; body = `export::result_markdown`. Nothing reaches the store before `SessionClosed` (§7.3); no in-progress metadata posts in the MVP |

- For review and verify, each accepted verdict has outcome `succeeded`. Its post becomes available when the following `SessionClosed` is applied, even if that event is in a separate batch. The body and structured fields use the session state at that close. A first publish after an extension reconstructs each round's post from its own close.
- For discussion and competition, `n` counts `SessionClosed` events in log order starting at 1. Each post snapshots the result at that event. After an extension, the next close uses the preceding close's acknowledged `postId` as `supersedesPostId`. A valid outbox entry with the legacy event ID `closed` represents the first close and retains its original request ID on retry.
- `requestId = <ns>-<sessionId>-<eventId>`, `sourceRef = {namespace: <ns>, sessionId, runId: <widget run or session>, eventId}` — deterministic, so a restart rebuilds the same envelope from the event log and the server collapses re-sends (key A and key B).
- State per candidate `pending | published{postId, seq} | failed{kind, message}` is kept in the widget's outbox dir `<data>/noa/timeline-outbox/<ns>/<sessionId>/` (`keep_published = true`) and shown in the session header/detail as `タイムライン: 投稿待ち / 投稿済み #seq / 投稿失敗（再試行）`; a 再試行 action re-sends the same envelope. Publishing is on the runner thread, after the source event is persisted; its failure never edits the session or its result.
- The saved `{boardId, runId}` is reused on restart (`registerRun` with the same `requestId <ns>-<sessionId>-run` and the saved `boardId`, which rebinds the new board-tab pane as root).
- discussion and competition: when the control server is off or lacks `boardTimelineV1`, nothing is registered, no outbox dir is made and the header shows nothing; their outbox dir is made only once a run is registered. A run is registered only from a board pane or with the saved `boardId`: a closed session with neither registers nothing (a run with neither would get a rootless board unrelated to the session) and its header shows `タイムライン: 投稿待ち` with `ボードに登録されていないため送れません`.
- discussion: a post whose minutes items exceed `MAX_LIST_ITEMS` per list, or whose structured fields alone exceed the 64 KiB params cap, gives up items from the end of the longer of `openIssues`/`nextActions`, and its body ends in a note that items were left out (the full result stays in 「結果をコピー」).

Tests per widget (`tests/timeline_*.rs` or inline): `ac5_candidates_from_confirmed_events_only`, `ac5_request_ids_stable_across_replay`, `ac5_restart_resends_exactly_once` (fake `TimelineApi`: first post times out, restart, second attempt → `duplicate: true`; store-level count 1), `ac5_failure_keeps_source_result` (session file bytes unchanged), `ac5_state_visible` (header text), competition `ac5_no_post_before_session_closed` (feed every event up to the last round; candidates empty). Publisher tests in noa-ipc: `stage_then_ack_marks_published`, `stage_conflict_refused`, `remove_after_ack_mode`, `resend_pending_uses_stored_envelope`.

### AC-6 Perf

`crates/noa-app/src/timeline/bench.rs`, `#[test] #[ignore] fn ac6_ten_thousand_posts()` (a new measurement, not a check: `#[ignore]` keeps a multi-second 10 000-post build out of `cargo test`, as the repo's existing ignored perf tests do, e.g. `crates/noa-app/src/auto_approve.rs:4020-4024`; it asserts nothing and weakens no existing test) — builds a board of 10 000 posts (bodies 0.5–8 KiB, seeded), then measures: first `list` (head, 20), a past page (`beforeSeq` = 5000), `list` with `query` hitting ~1%, cold replay (new `Store::open`), and peak RSS (`libc::getrusage(RUSAGE_SELF).ru_maxrss` before/after; `crates/noa-app/src/memory.rs` has no RSS reader, only `release_reclaimable_memory` :24). Prints one JSON line. Run: `cargo test --offline -p noa-app --release timeline::bench -- --ignored --nocapture`. Numbers are recorded in this file's Measurements section by the integrate step. No threshold.

### AC-7 Gate

No new dependency, no `Cargo.lock` change in the workspace or any widget (every crate used is already a dependency of the crate using it: V12/V17). `noa-ipc` stays GUI-free (no `wgpu`/`winit`), `noa-config` unchanged in deps. Baseline: `scripts/verify.sh --full` on 3d6ed594 before the first unit; the integrated branch shows no failure absent from the baseline. AC-8 (GUI look) stays manual.

## L2: Detail

### L2-1. Files and owners

Each file has exactly one owning unit. "new" = created by the unit. U1 creates surfaces that later units fill; a later unit edits only the files listed for it.

```text
crates/noa-ipc/src/timeline.rs              new  U1  wire types, TimelineCall, errors, validation, canon, Seq
crates/noa-ipc/src/timeline_outbox.rs       new  U1 surface (todo!() bodies) → U2 bodies
crates/noa-ipc/src/error.rs                      U1  ErrorCode -32012…-32018
crates/noa-ipc/src/protocol.rs                   U1  CAPABILITY_BOARD_TIMELINE_V1 const (not in SERVER_CAPABILITIES)
crates/noa-ipc/src/backend.rs                    U1  IpcBackend::timeline, ::capabilities (defaults)
crates/noa-ipc/src/client.rs                     U1  has_capability, supports_board_timeline, 8 wrappers
crates/noa-ipc/src/lib.rs                        U1  pub mod timeline, timeline_outbox; re-exports
crates/noa-config/src/lib.rs                     U1  timeline_boards_dir(_in), timeline_outbox_dir(_in), timeline_visits_path(_in)
crates/noa-app/src/lib.rs                        U1  mod timeline;
crates/noa-app/src/timeline/mod.rs          new  U1 surface → U3
crates/noa-app/src/ctl.rs                        U1  mods, Command::Timeline, parse/execute arms, `send --new-run`,
                                                     `board --timeline --board-id`, run_attr hook calls, USAGE
crates/noa-app/src/ctl/timeline.rs          new  U1 surface (parse/execute returning "not built") → U4
crates/noa-app/src/ctl/run_attr.rs          new  U1 surface (no-op bodies: register → Ok(None)) → U6
crates/noa-app/src/ctl/board.rs                  U1  only: `pub(crate) struct Open`, `run(.., open: Open)` signature,
                                                     `mod timeline;`; then U5
crates/noa-app/src/ctl/board/timeline.rs    new  U1 empty module → U5
crates/noa-ipc/src/server.rs, crates/noa-ipc/tests/server_tests.rs                         U2
crates/noa-app/src/timeline/{store,ids,replay,service,search,bench,tests}.rs (new)         U3
crates/noa-app/src/ipc_bridge.rs, crates/noa-app/src/app/ipc.rs                            U3
crates/noa-app/src/ctl/timeline.rs (bodies)                                                U4
crates/noa-app/src/ctl/board.rs (after U1), crates/noa-app/src/ctl/board/timeline.rs       U5
crates/noa-app/src/ctl/run_attr.rs (bodies), ctl/retry.rs, ctl/journal.rs, ctl/board/insight.rs   U6
widgets/{tasks,review,verify}/src/timeline.rs (new), their live.rs/live/runner.rs/live/start.rs,
  lib.rs, the ui file that draws the session header, tests/timeline_*.rs                   U7
widgets/{discussion,competition}/... same set                                              U8
docs/specs/noa-server.md (method table), docs/feature-map.md (row)                         U2 (docs part)
```

The U1→U5 hand-over of `board.rs` is sequential (U5 starts after U1 merged); no two units of one wave share a file. A unit that finds it needs another file stops that part and reports `#TODO(agent): <file and why>`.

### L2-2. Wire contract (`crates/noa-ipc/src/timeline.rs`, U1, frozen)

```rust
pub const CAPABILITY_BOARD_TIMELINE_V1: &str = "boardTimelineV1"; // in protocol.rs
pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_TITLE_CHARS: usize = 120;
pub const MAX_SUMMARY_CHARS: usize = 400;
pub const MAX_BODY_BYTES: usize = 32 * 1024;
pub const MAX_POST_PARAMS_BYTES: usize = 64 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 192 * 1024;
pub const MAX_BOARD_BYTES: u64 = 100 * 1024 * 1024;
pub const LIST_DEFAULT: usize = 20;   pub const LIST_MAX: usize = 50;
pub const CHANGES_DEFAULT: usize = 100; pub const CHANGES_MAX: usize = 200;
pub const MAX_TOKEN_BYTES: usize = 128; // requestId, producerId, sessionId, eventId, label (artifact, author, run)
pub const MAX_LIST_ITEMS: usize = 100;  // openIssues, nextActions, checks, artifacts, basedOnPostIds (each)

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Seq(pub u64);                 // serde: decimal string, like WireId; a JSON number is refused

pub enum IdKind { Board, Worker, Run, Post }   // prefixes "bd-", "wk-", "rn-", "po-"
pub fn valid_id(kind: IdKind, s: &str) -> bool; // ^<prefix>[0-9a-f]{16}$
pub fn valid_token(s: &str) -> bool;            // 1..=128 bytes of [A-Za-z0-9._:/-]

#[serde(rename_all = "snake_case")] pub enum PostKind { Summary, Milestone, Blocker, Decision, Handoff }
#[serde(rename_all = "snake_case")] pub enum Outcome { Succeeded, Partial, Failed, Cancelled, Unknown }
#[serde(rename_all = "snake_case")] pub enum CheckResult { Passed, Failed, NotRun, Inconclusive }
#[serde(rename_all = "snake_case")] pub enum PublishOwner { Worker, Runner, Manual }
#[serde(rename_all = "snake_case")] pub enum ProducerKind { Worker, Runner, Human }
#[serde(rename_all = "snake_case")] pub enum Provenance { Registered, Unverified }

pub struct SourceRef { pub namespace: String, pub session_id: String, pub run_id: Option<String>,
                       pub event_id: String, pub locator: Option<String> }
pub struct TaskRef   { pub kind: String /* local|github|other */, pub id: String,
                       pub url: Option<String>, pub title: Option<String> }
pub struct Artifact  { pub kind: String /* file|commit|pr|url|other */, pub label: String,
                       pub r#ref: String, pub digest: Option<String> }
pub struct Check     { pub name: String, pub result: CheckResult, pub reported_by: String,
                       pub evidence_ref: Option<String> }
pub struct Item      { pub text: String, pub r#ref: Option<String> } // deserialises from "text" too
pub struct AuthorHint { pub role: Option<String>, pub label: Option<String> }

pub struct PostInput {                   // client-supplied; the only part of a post a client sends
    pub schema_version: u32, pub kind: PostKind, pub outcome: Outcome,
    pub title: String, pub summary: String, #[serde(default)] pub body_markdown: String,
    pub occurred_at: Option<String>,     // RFC 3339 UTC "YYYY-MM-DDTHH:MM:SS[.fff]Z", a real date
    pub author: Option<AuthorHint>,      // only honoured when runId is null; serialised as null when absent
    pub source_ref: SourceRef, pub task_ref: Option<TaskRef>, pub retry_of_run_id: Option<String>,
    #[serde(default)] pub artifacts: Vec<Artifact>, #[serde(default)] pub checks: Vec<Check>,
    #[serde(default)] pub open_issues: Vec<Item>, #[serde(default)] pub next_actions: Vec<Item>,
    #[serde(default)] pub based_on_post_ids: Vec<String>, pub supersedes_post_id: Option<String>,
}
impl PostInput { pub fn validate(&self) -> Result<(), TimelineError>; pub fn normalized(self) -> Self; }
pub fn canon_bytes(run_id: Option<&str>, producer_id: Option<&str>, post: &PostInput) -> Vec<u8>;

pub struct Author   { pub worker_id: Option<String>, pub agent_kind: Option<String>,
                      pub role: Option<String>, pub label: Option<String> }
pub struct Producer { pub id: String, pub kind: ProducerKind }
pub struct Post {                         // stored and returned by get
    pub schema_version: u32, pub post_id: String, pub board_id: String, pub seq: Seq,
    pub created_at: String, pub request_id: String, pub producer: Producer,
    pub worker_id: Option<String>, pub run_id: Option<String>, pub author: Author,
    pub provenance: Provenance, #[serde(flatten)] pub input: PostInput, // flattened without input.author:
}                                         // the key `author` is the resolved Author, written once
pub struct PostSummary {                  // list / changes; no body
    pub post_id: String, pub seq: Seq, pub created_at: String, pub occurred_at: Option<String>,
    pub kind: PostKind, pub outcome: Outcome, pub title: String, pub summary: String,
    pub author: Author, pub producer: Producer, pub worker_id: Option<String>, pub run_id: Option<String>,
    pub task_ref: Option<TaskRef>, pub provenance: Provenance, pub source_namespace: String,
    pub checks: CheckCounts, pub open_issues: u32, pub artifacts: u32,
    pub supersedes_post_id: Option<String>, pub superseded_by: Vec<String>,
}
pub struct CheckCounts { pub passed: u32, pub failed: u32, pub not_run: u32, pub inconclusive: u32 }

pub struct PublishPolicy { pub owner: PublishOwner, pub producer_id: Option<String> } // worker: omitted → workerId;
                                                                                         // runner, manual: required
pub struct RunInfo { pub run_id: String, pub board_id: String, pub worker_id: Option<String>,
                     pub created_at: String, pub task_ref: Option<TaskRef>, pub retry_of_run_id: Option<String>,
                     pub publish_policy: PublishPolicy, pub summary_post_id: Option<String> }

pub struct ContextParams  { pub pane_id: Option<WireId>, pub board_id: Option<String> } // exactly one
pub struct ContextWorker  { pub worker_id: String, pub label: Option<String>, pub agent_kind: Option<String>,
                            pub pane_id: Option<WireId> /* live only */, pub surface_id: Option<String>,
                            pub live: bool, pub runs: Vec<RunInfo> }
pub struct ContextSelf    { pub pane_id: WireId, pub role: String /* root|worker */,
                            pub worker_id: Option<String>, pub runs: Vec<RunInfo> }
pub struct ContextResult  { pub board_id: String, pub name: String, pub server_instance_id: String,
                            pub archived: bool, pub head_seq: Seq, pub me: Option<ContextSelf>, // wire "self"
                            pub workers: Vec<ContextWorker> }
pub struct BoardsParams   { pub before: Option<BoardCursor> /* absent = from the newest */,
                            pub limit: Option<usize> /* ≤200, default 50 */ }
pub struct BoardInfo      { pub board_id: String, pub name: String, pub head_seq: Seq, pub created_at: String,
                            pub last_activity_at: Option<String>, // greatest createdAt of its posts; never decreases
                            pub archived: bool, pub posts: u64, pub bytes: u64 } // createdAt, lastActivityAt: RFC 3339 UTC
impl BoardInfo { pub fn activity_at(&self) -> &str /* lastActivityAt, else createdAt */; pub fn cursor(&self) -> BoardCursor; }
pub struct BoardCursor    { pub activity_at: String, pub board_id: String } // a position, not a reference
impl BoardCursor { pub fn admits(&self, board: &BoardInfo) -> bool; } // board comes after this position
pub fn board_listing_order(a: &BoardInfo, b: &BoardInfo) -> Ordering;  // activityAt desc (as instants), boardId asc
pub struct BoardsResult   { pub boards: Vec<BoardInfo>, pub has_more: bool,
                            pub next_before: Option<BoardCursor> /* last returned board's cursor */ }
pub struct RegisterRunParams { pub request_id: String, pub board_id: Option<String>, pub board_name: Option<String>,
                            pub orchestrator_pane_id: Option<WireId>, pub worker_pane_id: Option<WireId>,
                            pub task_ref: Option<TaskRef>, pub retry_of_run_id: Option<String>,
                            pub publish_policy: PublishPolicy, pub role: Option<String>, pub label: Option<String> }
pub struct RegisterRunResult { pub board_id: String, pub run_id: String, pub worker_id: Option<String>,
                            pub producer_id: String, pub created: bool }
pub struct SetPublishPolicyParams { pub board_id: String, pub run_id: String, pub request_id: String,
                            pub publish_policy: PublishPolicy }
pub struct PostParams     { pub board_id: String, pub run_id: Option<String>, pub producer_id: String,
                            pub request_id: String, pub post: PostInput }
pub struct PostResult     { pub post_id: String, pub seq: Seq, pub created_at: String, pub duplicate: bool }
pub struct Filter         { pub worker_ids: Vec<String>, pub agent_kinds: Vec<String>,
                            pub kinds: Vec<PostKind>, pub outcomes: Vec<Outcome> } // empty = any
pub struct ListParams     { pub board_id: String, pub before_seq: Option<Seq>, pub limit: Option<usize>,
                            pub filter: Option<Filter>, pub query: Option<String> /* ≤200 chars */ }
pub struct ChangesParams  { pub board_id: String, pub after_seq: Seq, pub limit: Option<usize> }
pub struct PageResult     { pub posts: Vec<PostSummary>, pub head_seq: Seq, pub has_more: bool,
                            pub next_before_seq: Option<Seq>, pub next_after_seq: Option<Seq> }
pub struct GetParams      { pub board_id: String, pub post_id: String }
pub struct GetResult      { pub post: Post, pub superseded_by: Vec<String> }

pub enum TimelineCall { Context(ContextParams), Boards(BoardsParams), RegisterRun(RegisterRunParams),
    SetPublishPolicy(SetPublishPolicyParams), Post(PostParams), List(ListParams), Get(GetParams),
    Changes(ChangesParams) }
impl TimelineCall {
    pub fn parse(method: &str, params: serde_json::Value) -> Result<Option<Self>, TimelineError>; // None = not a timeline method
    pub fn scope(&self) -> Scope;           // read | control as in the AC-1 table
    pub fn is_write(&self) -> bool;         // RegisterRun, SetPublishPolicy, Post
    pub fn request_id(&self) -> Option<&str>;
}
```

All structs: `#[serde(rename_all = "camelCase")]`, fields in sorted key order (repo convention, `protocol.rs`), `deny_unknown_fields` only on `PostInput` (so a typo in a hand-written summary is refused, not dropped). `TimelineCall::parse` refuses non-object `params`, and for `post` reads `post.schemaVersion` before decoding: an integer other than 1 is `unsupported` even when the post also has fields this version does not know.

### L2-3. Errors

| kind (`data.kind`) | code | `ErrorCode` variant | when |
|---|---|---|---|
| `invalid_params` | -32602 | `InvalidParams` (existing) | shape, limits, unknown references, producer mismatch |
| scope | -32003 | `ScopeDenied` (existing) | missing read/control (no `data`, as today) |
| `unsupported` | -32018 | `Unsupported` (new) | backend has no service; `schemaVersion ≠ 1` (checked before the post is decoded) |
| (method missing) | -32601 | `MethodNotFound` (existing) | old server or unknown `noa.timeline.*` |
| `outcome_unknown` | -32009 | `OutcomeUnknown` (existing) | write accepted to the queue, no reply within 4 s; `data.requestId` |
| `conflict` | -32012 | `TimelineConflict` (new) | idempotency or one-summary rule; `data.existingPostId` / `existingRunId` |
| `not_found` | -32013 | `TimelineNotFound` (new) | board/post/run/pane binding unknown |
| `busy` | -32014 | `Busy` (new) | queue full, read timeout, board locked by another instance |
| `storage_full` | -32015 | `StorageFull` (new) | append would exceed `MAX_BOARD_BYTES` |
| `io_error` | -32016 | `StoreIo` (new) | write/sync/rename failed; nothing acked |
| `corrupt_store` | -32017 | `CorruptStore` (new) | complete unparsable line or seq regression; `data.line` (1-based) |

-32012…-32018 are unused in the repo (searched `crates/`, `widgets/`, `docs/`). `pub struct TimelineError { kind: TimelineErrorKind, message: String, request_id, existing_post_id, existing_run_id, line }`, `TimelineErrorKind::{code, as_str, from_code}`. U2 extends the private `RpcFail` with `data: Option<Value>` and `error_response` writes it when present.

### L2-4. Store (`crates/noa-app/src/timeline/`, U3)

```text
<data>/noa/boards/                 0700   noa_config::timeline_boards_dir()
  <boardId>/                       0700
    meta.json                      0600   atomic replace + dir fsync
    posts.jsonl                    0600   append-only, one PostLine per line
    lock                           0600   flock(LOCK_EX|LOCK_NB), held while the service has the board open for writing
    quarantine/<ms>.tail           0600   torn tail bytes moved aside on repair
```

- `meta.json`: `{schemaVersion, boardId, name, createdAt, roots: [Binding], workers: {wk-…: {label, agentKind, role, bindings: [Binding]}}, runs: {rn-…: {requestId, order, canon: <normalised client fields as a JSON object>, info: RunInfo, policyHistory: [{at, policy, requestId}]}}, requestAliases: {<producerId>: {<requestId>: <postId>}}}` with `Binding = {serverInstanceId, paneId, surfaceId, at}`. The run canon holds only client-supplied fields (`boardId` as given, `taskRef`, `retryOfRunId`, `publishPolicy`, `role`, `label`), never pane ids, so a widget re-registering from a new board-tab pane after a restart gets the same run (§6.1 "再送時に変わる値を…付け足さない"). `order` (1-based, per board) is the registration order `context` lists runs in: `createdAt` can go backwards with the clock and ties within a millisecond. `requestAliases`: AC-1 rule 4's key-B duplicates.
- `registerRun` with a known `requestId` answers with its run before any pane is looked up, so a re-send after the worker or orchestrator closed still gets the run (an orchestrator that is open is rebound as a root). The request index is checked again after the board the call names is read and after the board's lock is taken, since another instance may have registered the request; every `meta.json` change is made on the copy read under the lock.
- `PostLine = {"v":1, "requestId", "canon": <normalised canon object>, "post": Post}`.
- Write path (§8.1): validate → lock held (else `busy`) → `writable` (no poison, no corruption) → size check → id = `po-` + 16 hex from `rand::thread_rng` (re-drawn on collision with the in-memory id set) → seq = head+1 → one `write_all(line + "\n")` → `sync_data` → then update in-memory indexes (A, B, post ids, summaries with byte offsets) → reply. A failed write/sync marks the board `poisoned`; the next call truncates to the last durable length (`set_len`), re-replays, then proceeds; a replay that fails to read leaves the board as it was (still poisoned) instead of half cleared. Taking the lock re-reads what the previous holder wrote; the lock is kept only once that read succeeded, so a writer never works from a stale index. First creation of `posts.jsonl`/`lock`/`meta.json` fsyncs the board dir and `boards/`; creating `boards/` fsyncs the parent of every directory it adds (`noa/` in the data dir on a first launch, and `boards/` in `noa/`).
- Replay (`replay.rs`): read the file; every `\n`-terminated line must parse whole, as the stored types the store later reads it as (`canon` as the stored canon, `post` as `Post`), and have seq = previous + 1, else `corrupt` at that line (board read-only, `corrupt_store` on writes, reads serve the valid prefix). A final unterminated segment is torn: under the lock it is copied to `quarantine/`, the file is `set_len` to the last `\n`, `sync_data`; that post was never acked, so its re-send creates it.
- Reads open the file read-only and never need the lock; a second Noa instance can browse a board another instance writes.
- Live bindings: `register_run` records a `Binding` for root and worker from the panels snapshot passed with the job (`surface_id`, `label`, `agent.kind` from `Panel`). `live` = binding.serverInstanceId == `noa_ipc::server::server_instance_id()` && a panel in the snapshot has that `paneId` && equal `surfaceId`.
- `StoreLimits { max_board_bytes }` and a `StoreIo` trait (write, sync, rename, directory sync, the board `flock`, the replay read) are injectable for tests.

### L2-5. Service and dispatch (U3 app side, U2 server side)

```rust
// crates/noa-app/src/timeline/service.rs
pub(crate) struct TimelineHandle { tx: crossbeam_channel::Sender<Job> }
pub(crate) struct Job { call: TimelineCall, panels: Arc<Vec<Panel>>, reply: crossbeam_channel::Sender<Result<Value, TimelineError>> }
pub(crate) fn start(root: PathBuf) -> io::Result<TimelineHandle>;       // thread "noa-timeline", channel bounded(64); no file I/O on the caller
impl TimelineHandle { pub(crate) fn call(&self, call: TimelineCall, panels: Arc<Vec<Panel>>) -> Result<Value, TimelineError>; } // try_send → busy; recv_timeout(4 s) → outcome_unknown (write) / busy (read)

// crates/noa-ipc/src/backend.rs (U1)
fn timeline(&self, _call: TimelineCall) -> Result<serde_json::Value, TimelineError> { Err(TimelineError::unsupported()) }
fn capabilities(&self) -> Vec<&'static str> { Vec::new() }
```

- `AppIpcBackend` gains `timeline: Option<TimelineHandle>` (`ipc_bridge.rs:372`); `timeline()` clones `self.shared.lock().panels` and calls the handle; `capabilities()` returns `[CAPABILITY_BOARD_TIMELINE_V1]` when `Some` and its store opened (`TimelineHandle::opened()`); otherwise `timeline()` answers `unsupported`.
- `App::install_ipc_server_if_needed` (`app/ipc.rs:29`) starts the service with `noa_config::timeline_boards_dir()`; a start failure logs once and leaves `None` (capability absent). `start` only spawns the thread (D6: no file I/O on the UI thread): the service creates `boards/` (`0700`) and reports at once whether that worked, which `TimelineHandle::opened()` waits for (up to 4 s, on the connection thread at `noa.hello`); a store that cannot be opened (e.g. `boards/` on a read-only volume, where the `chmod` fails) logs once and leaves the capability out. The boards are replayed after that answer, while calls already queue; a failed first directory listing is retried by the next call.
- `server.rs` (U2): in `dispatch`, before the `_ => MethodNotFound` arm (:1587), `m if m.starts_with("noa.timeline.")` → `TimelineCall::parse` (None → -32601) → `require_scope(session, call.scope())` → for `Post`, the raw params' encoded length ≤ `MAX_POST_PARAMS_BYTES` → `backend.timeline(call)` → result or `RpcFail` with `data`. `handle_hello` gets `&dyn IpcBackend` and appends `backend.capabilities()` after `SERVER_CAPABILITIES`.

### L2-6. Run attribution (`ctl/run_attr.rs`, U1 surface, U6 bodies)

```rust
pub(crate) trait TimelineApi { fn register_run(&mut self, p: &RegisterRunParams) -> Result<RegisterRunResult, ClientError>;
                               fn context(&mut self, p: &ContextParams) -> Result<ContextResult, ClientError>;
                               fn supports(&self) -> bool; }               // impl for noa_ipc::Client
pub(crate) struct RunAttr { pub board_id: String, pub run_id: String, pub worker_id: Option<String>,
                            pub producer_id: String, pub retry_of: Option<String> }
pub(crate) enum NewRun { SpawnTask, SendNewRun, Retry { of_run: Option<String> } }
pub(crate) fn register(api: &mut dyn TimelineApi, orchestrator: Option<u64>, worker_pane: u64,
                       new: NewRun, request_id: &str) -> Result<Option<RunAttr>, String>; // None: no capability
pub(crate) fn new_request_id() -> String;                     // "ctl-" + 16 hex
pub(crate) fn header(attr: &RunAttr) -> String;
pub(crate) fn with_header(attr: Option<&RunAttr>, text: &str) -> String;
pub(crate) fn strip_header(text: &str) -> &str;               // then retry::strip_preface
pub(crate) fn journal_fields(attr: &RunAttr) -> serde_json::Value; // {"boardId","runId","workerId"}
pub(crate) fn choose_run<'a>(runs: &'a [RunInfo], explicit: Option<&'a str>) -> Result<&'a str, String>;
```

Header (exactly one line, then a blank line; placed before retry's preface):

```text
[noa-timeline board=bd-0123456789abcdef run=rn-0123456789abcdef] When done, publish a summary: "$NOA_BIN" +ctl timeline post --run rn-0123456789abcdef --file <summary.json>
```

U1's hooks in `ctl.rs`: `Session::spawn` (before `self.deliver`, only when a task is present) and the `send --new-run` path call `run_attr::register` and `with_header`; `Session::register_run` journals each registered run as its own `{"ev":"run","pane",…journal_fields}` event, and `spawn` also merges `journal_fields` into its printed line (the `spawn` journal event is unchanged; U6 folds `run` events into `Worker.run_id`). `send --new-run` goes through `ctl::new_run_text`, which runs `send_checks` (the pre-typing part of `send_steps`) on the text under a header of a run's length before it registers. `Session::retry` passes `NewRun::Retry { of_run: worker.run_id.clone() }`. With U1's no-op body (`Ok(None)`) the typed text is unchanged, so existing ctl tests keep passing until U6.

### L2-7. Publisher (`crates/noa-ipc/src/timeline_outbox.rs`, U1 surface, U2 bodies)

```rust
pub struct Envelope { pub board_id: String, pub run_id: Option<String>, pub producer_id: String,
                      pub request_id: String, pub post: PostInput }
impl Envelope { pub fn key(&self) -> EnvelopeKey; }
pub struct EnvelopeKey { pub board_id: String, pub producer_id: String, pub request_id: String } // §6.1 key A; Hash + Eq
pub enum PublishState { Pending, Published { post_id: String, seq: Seq }, Failed { kind: Option<TimelineErrorKind>, message: String } }
pub struct Entry { pub envelope: Envelope, pub state: PublishState, pub attempts: u32, pub updated_at_ms: u64 }
pub struct RunBinding { pub board_id: String, pub run_id: String, pub producer_id: String }
pub struct Outbox { /* dir, keep_published */ }
impl Outbox {
    pub fn open(dir: PathBuf, keep_published: bool) -> io::Result<Self>;        // 0700
    pub fn stage(&self, env: &Envelope) -> Result<Entry, OutboxError>;           // durable before return; same key+same env → existing; differs → Conflict
    pub fn get(&self, key: &EnvelopeKey) -> io::Result<Option<Entry>>;
    pub fn entries(&self) -> io::Result<Vec<Entry>>;
    pub fn record(&self, key: &EnvelopeKey, state: PublishState) -> io::Result<()>; // removes the file on Published when !keep_published
    pub fn save_run(&self, run: &RunBinding) -> io::Result<()>;  pub fn load_run(&self) -> io::Result<Option<RunBinding>>;
}
pub trait PostSink { fn post(&mut self, p: &PostParams) -> Result<PostResult, ClientError>; } // impl for Client; fakes in tests
pub fn publish(sink: &mut dyn PostSink, outbox: &Outbox, env: Envelope) -> PublishState;
pub fn resend_pending(sink: &mut dyn PostSink, outbox: &Outbox) -> Vec<(EnvelopeKey, PublishState)>;
```

File per entry `<fnv1a64(boardId \0 producerId \0 requestId) hex>.json`, 0600, staging + `sync_all` + rename + dir fsync. Never stores a token or URL (§8). FNV-1a is written inline (a dozen lines, as `widgets/tasks/src/store.rs:33` already does for repo keys).

### L2-8. CLI (`ctl/timeline.rs`, U4)

```rust
pub(crate) enum TimelineCommand { Context { pane: Option<u64> }, Boards { before: Option<BoardCursor>, limit: Option<usize> },
    Post(PostArgs), List { board: Option<String>, before: Option<u64>, limit: Option<usize>, filter: Filter, query: Option<String> },
    Show { board: Option<String>, post: String }, Changes { board: Option<String>, after: u64, limit: Option<usize> },
    Export { board: Option<String>, format: ExportFormat, output: PathBuf } }
pub(crate) fn parse(args: &[String]) -> Result<TimelineCommand, String>;
pub(crate) fn execute(client: &mut noa_ipc::Client, own: Option<u64>, cmd: TimelineCommand) -> Result<i32, super::Failure>;
pub(crate) fn read_result_file(path: &Path, cap: usize) -> io::Result<Vec<u8>>;
pub(crate) fn export_markdown(board: &ContextResult, posts: &[Post]) -> String;
pub(crate) fn export_json(board: &ContextResult, posts: &[Post]) -> String;
```

`execute` goes through a small trait (`TimelineApi` extended with list/get/changes/post/boards/set_publish_policy) so tests use a fake.

### L2-9. Board model (`ctl/board/timeline.rs`, U5)

```rust
pub(crate) struct Open { pub timeline: bool, pub board_id: Option<String> }   // in board.rs (U1)
pub(crate) struct Timeline { board_id: Option<String>, posts: Vec<PostSummary> /* desc */, pending: Vec<PostSummary>,
    head_seq: u64, recv_cursor: u64, visit_boundary: u64, selected: usize, scroll: usize,
    filter: Filter, query: Option<String>, input: Option<InputLine>, detail: Option<(String, Option<GetResult>)>,
    live: HashMap<String /* workerId */, LiveWorker>, last_ok: Option<Instant>, stale: bool, unsupported: bool }
pub(crate) enum TlAction { Fetch(GetParams), Page(ListParams), Focus(u64), ShowWorker(u64), SaveVisit(u64) }
impl Timeline {
    pub(crate) fn on_page(&mut self, page: PageResult);          // list result (past page or reload)
    pub(crate) fn on_changes(&mut self, page: PageResult, now: Instant);
    pub(crate) fn on_context(&mut self, ctx: &ContextResult, listing: &[Panel], same_instance: bool);
    pub(crate) fn on_failure(&mut self, now: Instant);
    pub(crate) fn key(&mut self, key: Key) -> Option<TlAction>;
    pub(crate) fn since_last_visit(&self) -> usize;
    pub(crate) fn render(&self, cols: usize, rows: usize, now: Instant) -> Vec<String>;
}
```

`board.rs` owns the I/O: it calls `context` and `changes` every 2 s when the server supports the capability (tracked with `Polling` like `LIST_EVERY`), runs `TlAction`s, and reads/writes `timeline-visits.json`.

## Risks

- R1 — one service thread serialises a big search behind a post. Bounded by D12 + AC-6 numbers; if a search over 10 000 posts exceeds ~200 ms, the follow-up is a search worker reading posts by offset (outside this run).
- R2 — two Noa processes (release + dev) on one data dir: the second gets `busy` for writes on boards the first holds; reads work. Boards are locked lazily on first write, so a second instance can still create its own boards.
- R3 — the 4 s service wait vs. the 2 s UI-thread `REQUEST_TIMEOUT`: the timeline path never goes through `submit`, so only the client's 5 s timeout bounds it; 4 s keeps the reply ahead of it.
- R4 — a header typed into an agent's prompt is visible to the agent and in screen captures; it carries ids only, no token.
- R5 — idempotency compares client fields only; a client that puts a clock value into `PostInput` would make re-sends conflict. The adapters build envelopes from persisted events only (L2-7), and the CLI default request id hashes the canonical payload.
- R6 — `retry` on a pane whose journal lacks `runId` (spawned before this feature) gets `retryOfRunId = null`; not guessed (§7.1).
- R7 — widget adapters span five crates with their own lockfiles; any accidental new import from noa-ipc that pulls a new crate changes those locks. U7/U8 check `git diff --stat -- '*.lock'` is empty.

## Open questions (defaults taken)

- Q1 — `registerRun` fails after retries during `spawn --task`: block the task or deliver without a header? **Default: deliver without a header and warn** (orchestration keeps working; the worker cannot publish, which is visible).
- Q2 — the 8th method `setPublishPolicy` (D10) is not in the proposal's table. **Default: add it** (the proposal requires an explicit recorded owner change and names no method for it).
- Q3 — tasks' "explicit share" needs a UI action that does not exist. **Default: a 共有 action in the run's result view, outcome chosen in a two-option form (default `unknown`).**
- Q4 — competition in-progress "提出済み" metadata posts. **Default: none in the MVP** (§7.3 allows but does not require them; none is the safer reading of "blind bodies never in the store").
- Q5 — where "前回閲覧以降" lives. **Default: client-side `<data>/noa/timeline-visits.json`** (human state, not shared, not on the service thread).
- Q6 — review/verify outcome semantics (is a `RequestChanges` review a `partial` work outcome?). **A session that reached a verdict has outcome `succeeded`. The verdict is a check** (`passed`, `failed`, or `inconclusive` for an inconclusive verification), as detailed in AC-5.
- Q7 — capacity limit of 100 MiB per board fixed in code (§8.1 says "configurable later"). **Default: constant, no config key in this run.**
- Q8 — an automated end-to-end test (`+ctl` ↔ live server ↔ store over loopback). **Default: none in this run**; units test through fakes, `integrate` checks the path by hand in a running Noa (loopback binding fails in the sandbox, CLAUDE.md).

## Not in this cut

Push notifications (`Notification` variant), LLM summaries, Agent Dashboard integration, comments/replies/reactions, strict read state, deleting archives (`storage_full` message points at `export` and removing the board dir by hand), multi-user sync/ACL.

## Implementation plan

```text
wave 1:  U1 contract
wave 2:  U2 ipc-server ∥ U3 store ∥ U4 ctl-cli ∥ U5 board-view ∥ U6 ctl-runs
wave 3:  U7 widgets-a (tasks, review, verify) ∥ U8 widgets-b (discussion, competition)
final:   integrate (merge only, AC-6 numbers into this file, AC-7 gate)
```

### Rules for every unit

- Edit only the files L2-1 assigns; L2-2/L2-5/L2-6/L2-7 surfaces are frozen (a needed change: stop, `#TODO(agent): <change and why>`).
- No new dependency, no `cargo update`, `--offline` (and `--locked` in widgets). `git diff --stat main -- Cargo.lock 'widgets/*/Cargo.lock'` must be empty.
- Tests named `acN_…`; `cargo clippy -p <crate> --all-targets` adds no warning.
- `noa-ipc` tests and anything binding loopback run with the sandbox off (CLAUDE.md, Sandbox constraints).

| Unit | Wave | Owns | Depends on | ACs | Done when |
|---|---|---|---|---|---|
| `contract` (U1) | 1 | L2-1 rows marked U1 | — | AC-1 wire part, surfaces for all | `cargo build --offline --workspace --all-targets`; `cargo test --offline -p noa-ipc --lib timeline` (seq decimal, canon golden, parse per method, limits); `cargo test --offline -p noa-config`; `cargo test --offline -p noa-app ctl::` unchanged count/pass (hooks are no-ops); each widget `cargo build --locked --offline --all-targets` |
| `ipc-server` (U2) | 2 | `noa-ipc/src/server.rs`, `timeline_outbox.rs` bodies, `tests/server_tests.rs`, `docs/specs/noa-server.md`, `docs/feature-map.md` | U1 | AC-1 (dispatch, scopes, `data.kind`, capability), AC-2/AC-5 outbox | `cargo test --offline -p noa-ipc` (sandbox off) incl. `timeline_` server tests and outbox tests; clippy clean |
| `store` (U3) | 2 | `noa-app/src/timeline/*`, `ipc_bridge.rs`, `app/ipc.rs` | U1 | AC-1 store/service, AC-6 | `cargo test --offline -p noa-app timeline::` (all `ac1_*`, `ac4_*` store tests); `cargo test --offline -p noa-app --release timeline::bench -- --ignored --nocapture` prints the JSON line |
| `ctl-cli` (U4) | 2 | `ctl/timeline.rs` | U1 | AC-2 | `cargo test --offline -p noa-app ctl::timeline` (all `ac2_*`) |
| `board-view` (U5) | 2 | `ctl/board.rs`, `ctl/board/timeline.rs` | U1 | AC-3 | `cargo test --offline -p noa-app ctl::board` (all `ac3_*` plus the existing 130) |
| `ctl-runs` (U6) | 2 | `ctl/run_attr.rs` bodies, `ctl/retry.rs`, `ctl/journal.rs`, `ctl/board/insight.rs` | U1 | AC-4 | `cargo test --offline -p noa-app ctl::` (all `ac4_*` ctl/run_attr/insight tests); the `ac4_*` ctl-level tests that need ctl.rs fixtures use the fake `TimelineApi` only |
| `widgets-a` (U7) | 3 | widgets tasks/review/verify files per L2-1 | U1, U2 | AC-5 (3 widgets) | in each of `widgets/tasks`, `widgets/review`, `widgets/verify`: `cargo build --locked --offline --all-targets && cargo test --locked --offline timeline` |
| `widgets-b` (U8) | 3 | widgets discussion/competition files per L2-1 | U1, U2 | AC-5 (2 widgets) | same in `widgets/discussion`, `widgets/competition`, incl. `ac5_no_post_before_session_closed` |
| `integrate` | final | merge only; Measurements section of this file | all | AC-6 record, AC-7 | `scripts/verify.sh --full` (sandbox off for noa-pty/noa-ipc) shows no failure absent from the 3d6ed594 baseline; `git diff --stat 3d6ed594 -- Cargo.lock 'widgets/*/Cargo.lock'` empty; `cargo tree -p noa-ipc -e normal` contains no `wgpu`/`winit` |

U4–U6 compile against U1's surfaces only and test through fakes (`TimelineApi`, `PostSink`). The only end-to-end path (`+ctl` → server → store in a running Noa) is checked by `integrate` by hand — `timeline post`, `list`, `changes`, `export`, then quit and relaunch Noa and `list` again — with the output recorded in its report; there is no automated loopback test for it in this run (open question Q8).

## Measurements

AC-6, `cargo test --offline -p noa-app --release timeline::bench -- --ignored --nocapture` on the integrated branch:

```json
{"bench":"ac6_ten_thousand_posts","buildMs":40295.0,"changesPageMs":0.42,"coldReplayMs":48.4,"fileBytes":99129148,"firstListMs":0.21,"pastPageMs":0.09,"peakRssAfterBuildBytes":28950528,"peakRssAfterReplayBytes":44662784,"peakRssStartBytes":11239424,"posts":10000,"queryHitCount":50,"queryHitMs":29.4,"queryMissFullScanMs":38.25}
```

The machine was under heavy load from parallel builds during this and the earlier runs, so the numbers are upper bounds. `queryMissFullScanMs` is the worst case for risk R1 (a search that matches nothing reads every post); all runs stay under the ~200 ms threshold except (a) below.

Earlier runs from the store unit:

- (a) at 3f129a34: `{"firstListMs":0.15,"pastPageMs":0.06,"coldReplayMs":63.99,"queryHitMs":57.17,"queryMissFullScanMs":323.78,"peakRssAfterReplayBytes":49135616,"fileBytes":99129148}`
- (b) at 35160a5d (load average ~35): `{"firstListMs":0.21,"pastPageMs":0.09,"coldReplayMs":129.13,"queryHitMs":45.15,"queryMissFullScanMs":92.01,"peakRssAfterReplayBytes":46301184}`

Each body is stored twice on disk (canon + post), so 10 000 posts take ~99 MB (`fileBytes` 99 129 148) against the 100 MiB per-board cap. This is an open owner decision.
