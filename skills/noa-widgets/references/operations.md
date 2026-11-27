# Widget operations

## Contents

- [Commands](#commands)
- [State and retry contract](#state-and-retry-contract)
- [Event processing](#event-processing)
- [Report processing](#report-processing)
- [Lifecycle and cleanup](#lifecycle-and-cleanup)
- [Limits and authority](#limits-and-authority)

## Commands

Prefix every command with `"$NOA_BIN" +ctl` in a Noa pane, or the selected
Noa executable followed by `+ctl` outside one.

| Command | Purpose |
| --- | --- |
| `widget schema` | Print the v1 editor JSON Schema; no server connection |
| `widget validate FILE [--data-file FILE]` | Validate definition, initial data, and optional supplied data offline |
| `widget preview FILE [--data-file FILE]` | Open a local preview without a control server |
| `widget preview FILE --data-file FILE --snapshot FILE.png --size 960x720` | Render a software PNG preview offline |
| `widget list [--repo DIR]` | List valid current-worktree definitions, package diagnostics and local instances |
| `widget describe ID [--repo DIR]` | Read one shared manifest and its digest |
| `widget open ID [--repo DIR] [--data-file FILE] --request-id ID` | Open a shared definition as a new instance |
| `widget open --definition-file FILE [--repo DIR] [--data-file FILE] --request-id ID` | Open an inline definition without writing a shared package |
| `widget get INSTANCE` | Get the pinned definition, current data and lifecycle state |
| `widget set INSTANCE --data-file FILE --expected-revision N --request-id ID` | Replace complete data with compare-and-swap |
| `widget events INSTANCE [--consumer ID] [--after N] [--limit N] [--wait 30s]` | Claim/read the single event stream |
| `widget ack INSTANCE --through N [--consumer ID] --request-id ID` | Acknowledge a contiguous handled prefix |
| `widget report INSTANCE --event ID --expected-version N --state STATE [--consumer ID] [--message TEXT] --request-id ID` | Report processing for a delivered event; requires `repoWidgetOperationsV1` |
| `widget close INSTANCE --request-id ID` | Stop the host while retaining state and events |
| `widget forget INSTANCE` | Delete a stopped instance with no pending events |

`FILE=-` reads JSON from stdin for `validate`, `open` and `set`. Manifest and
data cannot both consume stdin. Preview requires files. `--size` requires
`--snapshot`; use 320..4096 logical pixels per side, at most 8 million logical
pixels. PNG output is 2× the logical size. Inspect previews at normal size and
at a narrow width, for example 480×640. Preview buttons show an event locally;
they never perform the operation or submit a live event.

For new request/consumer IDs, use ASCII letters, digits, `.`, `_`, `:` and `-`,
at most 128 bytes. Generate an operation ID once, for example:

```sh
operation_id=$(python3 -c 'import uuid; print(uuid.uuid4().hex)')
consumer_id="$operation_id-agent"
```

Save these values in the task's local records before the first mutation.
Restore them instead of rerunning ID generation after reconnecting.

## State and retry contract

`open` and `get` snapshots include `instanceId`, `definitionId`,
`definitionDigest`, `definitionSource`, `repo`, `repositoryId`, `worktreeId`,
`dataRevision`, `data`, `manifest`, `status`, `hostReady`, `ownerPaneId`,
`consumerId`, `lastSeq`, `ackedThrough`, `dataUpdatedAtMs` and `operations`.
`open` also reports `created`.
The returned `instanceId` has a generated `wi_…` value; never substitute the
manifest ID for it.

The definition and its digest are fixed for an instance. Editing widget.json
affects subsequent opens. To use an edited definition, close the old instance
and open a new one under a new request ID.

`set` replaces the whole data object and increments `dataRevision`. A revision
mismatch changes nothing. An identical successful retry returns the original
result before checking its now-old revision; changing payload or expected
revision under the same request ID is a conflict. Keep a saved copy of each
in-flight operation's input.

`open` IDs are reserved across retained instances. Other mutation IDs are
remembered within the instance. Do not reuse IDs for unrelated operations.
After `forget`, the old open request's deduplication history no longer exists:
never retry that old open operation after forgetting its instance.

## Event processing

Each event has `eventId`, `seq`, `actionId`, `dataRevision`, and `payload`.
The response includes `events`, `nextSeq`, `lastSeq`, `ackedThrough`, `hasMore`,
`consumerId`, and `status`. The default page is 32; the maximum is 64.

Read with a stable explicit consumer ID. In a recognized Noa pane the CLI can
default to `pane:<pane-id>`; outside one, `--consumer` is required. Use an
explicit ID when a later process should continue the same task. There is no
consumer lease, reset, or takeover command in v1.

Use this processing order:

```text
read events and the current reported operations
for each event in seq order:
    look up eventId in the durable operation journal
    if completed: reuse the recorded result
    otherwise:
        if reports are supported: report running with the current version and a saved request ID
        perform the authorized operation and durably record its result
    if the instance is open: publish updated data if needed, using a saved request ID and CAS revision
    if reports are supported and no final report exists:
        report the recorded final outcome with the current version and a saved request ID
ack the highest contiguous handled seq
```

Persist downstream idempotency keys or check downstream state to resolve the
crash window between a side effect and writing its completion record. Do not
promise exactly-once effects from at-least-once delivery.

`--after N` may page beyond events already delivered to this consumer. It
cannot skip events that were never delivered. A request without `--after`
returns pending events after the server's acknowledged prefix; this is the
preferred recovery read. `nextSeq` means delivered, not completed. `ack
--through N` discards every pending event through N, so never acknowledge over
an unfinished earlier event.

`--wait` accepts 0..300 seconds. It polls and returns on an event, a stopped
instance, or timeout. It does not schedule work or resume an agent later.
Use a wait shorter than the enclosing tool's timeout.

## Report processing

Check that the server advertises `repoWidgetOperationsV1` before using
`report`; the CLI reports a clear unsupported-capability error on older
servers. Older producers still work, but the host cannot confirm their
processing outcomes from queue acknowledgments or data changes.

Each accepted UI action has an operation at version 1. Match it by `eventId`
in `widget get`'s `operations`, which are ordered by `seq`. Save the report
request ID, exact params and resulting version with the event's task records.
Use the same explicit consumer that received the event. Never guess a new
identity to bypass a consumer conflict.

| State | Use |
| --- | --- |
| `accepted` | The server queued the event; the producer has not reported processing. Producers cannot assign this state. |
| `running` | Processing has started. |
| `waiting` | Processing awaits a response or prerequisite; explain it in `message`. |
| `completed` | The operation finished and its outcome was recorded. |
| `failed` | The operation ended unsuccessfully or was intentionally declined; record the reason. |

For a newly delivered event, before any report has been accepted:

```sh
noaw report "$instance" --consumer "$consumer_id" \
  --event "$event_id" --expected-version 1 --state running \
  --message 'Running the requested check' \
  --request-id "$operation_id-$event_seq-started"
# Save the returned version (2). After recording the actual result:
noaw report "$instance" --consumer "$consumer_id" \
  --event "$event_id" --expected-version 2 --state completed \
  --message 'Check finished; results are recorded' \
  --request-id "$operation_id-$event_seq-completed"
```

These versions describe the example's first two reports. On recovery, read
the actual operation version and reuse any in-flight report's exact saved
request. Each accepted report increments the version. A stale version changes
nothing; read and reconcile it before issuing a new request. An uncertain
outcome requires an identical retry. `completed` and `failed` are final:
retrying the same report returns its original result, while a different
report cannot overwrite it. If a recovered operation is already final,
reconcile it with the durable task journal and do not repeat its side effects.

Messages are limited to 1,024 UTF-8 bytes. Keep them factual and avoid secrets.
The host displays the 32 latest operations and counts all operations lacking
a final outcome. `get` retains the complete bounded history, original UI
`actionRequestId` when known, and recorded Unix-millisecond timestamps.
Legacy queued events have unknown timestamps; older acknowledged events are
not reconstructed. A report records the consumer's claim, not independent
verification of the underlying effect.

Report final outcomes before acknowledgment in the normal processing loop.
Reports also work after acknowledgment or on a stopped instance, so they can
finish recovery without reopening a host. `ack`, `set`, timeout and window
closure never mark work complete. Report history and report request IDs share
the instance's existing lifetime storage and retry limits.

## Lifecycle and cleanup

| Status | Handling |
| --- | --- |
| `open` | Check `hostReady`; data may be updated and actions accepted |
| `closed` | Explicitly stopped; read/drain/ack, then forget when history is no longer needed |
| `hostExited` | Native host ended; retained state can be read/drained |
| `ownerExited` | Owner pane ended; its host was stopped and state retained |
| `restored` | Application restarted; old open state is retained without relaunching anything |

An instance opened outside a recognized pane has no owner-pane lifetime.
Close it explicitly when done. Stopped instances accept event reads,
acknowledgments and processing reports but cannot be updated or accept new actions. To continue,
open a new instance, optionally supplying the old data.

Close before the final drain so no further clicks can arrive. `forget` rejects
open instances and instances with pending events. Repeating `forget` for the
same generated ID is safe even if an earlier response was lost. Forgetting
deletes data, definition snapshot, events, processing reports and deduplication
history, including operations without a final outcome. Export anything still
needed first; forgetting is explicit cleanup and never proves completion.

## Limits and authority

The service retains at most 64 instances, with at most 16 open. Each instance
has at most 128 pending events (also bounded by 256 KiB) and separate 1,024-entry
mutation/action retry ledgers. Queues and ledgers are not silently evicted.
A full event queue rejects clicks until events are acknowledged. At a retry
ledger's lifetime limit, close, drain, and forget the finished instance, then
open another if the task continues.

Manifests are at most 64 KiB, current data 128 KiB, action payloads 16 KiB.
Validation also bounds schema/view depth, table rows, expanded content and
displayed text. Reduce data or the view when validation rejects a limit;
do not silently truncate evidence while reporting a complete result.

Public widget RPC uses Noa's existing trusted-controller model. `list`,
`describe`, and `get` need `read`; `open`, `set`, `events`, `ack`, `report`, `close`, and
`forget` need `control`. Repository, pane and consumer IDs coordinate state;
they are not per-repository authorization boundaries. Restrict agent behavior
to its own task and instances even though the controller API has wider reach.

V1 has no network collectors, arbitrary execution, embedded web content,
data migrations, hot reload, event broadcast, automatic data watchers or
automatic agent resumption. Keep collection and side effects in the agent's
normal tools, with the user's existing authorization.
