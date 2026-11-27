# Test summary

Display test results supplied by a coding agent and request a rerun of the
recorded failed tests. The package is a declarative example: it contains no
test runner, data collector, executable, or automatic polling process.

## Data contract

| Field | Meaning |
| --- | --- |
| `summary` | A human-readable description of the result, collection failure or waiting state; at most 2,000 characters |
| `passed` | Number of tests the producer actually observed passing |
| `failed` | Number of tests the producer actually observed failing |
| `failures` | Up to 200 failed-test records; each has a stable, nonempty `id` and a display `name` |

Provide a complete data object on each update. `id` identifies a test across
runs; `name` is display text and must never be evaluated as a shell command.
An empty failures array by itself does not prove success. If the runner could
not execute, explain that in `summary`; do not present zeros as a successful
test result. If more than 200 tests fail, report the observed total in `failed`,
show a bounded selection in `failures`, and state that selection in `summary`.

## Action: rerun-failed

The button emits `actionId: "rerun-failed"` with payload `{}` and the displayed
`dataRevision`. The responsible agent should:

1. Read the event using its stable consumer ID and deduplicate by `eventId`.
2. Find the failed-test IDs in the producer's record for that data revision.
   If that record is unavailable, do not silently substitute a newer set of
   tests; report or record that the requested rerun could not be resolved.
3. Run only the selected tests using the repository's existing test tooling
   and the task's authorization. Construct arguments from known test IDs and
   the runner's supported selection syntax, not from display text.
4. Record the result, publish the complete new data with compare-and-swap,
   and acknowledge the contiguous handled event sequence.

If the referenced failure list is empty, record that there is nothing to
rerun. A click does not grant new permissions. Offline preview shows the
action locally and never executes the test suite. The sample includes no
background agent that handles this button automatically.

## Validate and preview

Run from the repository root. Use `$NOA_BIN` in a Noa pane, the installed
`Noa`, or the selected build (`target/debug/Noa` when working on Noa itself).
Build the `widget-host` helper as described in
[Repository widgets](../../docs/REPOSITORY_WIDGETS.md) before previewing.

```sh
widget_noa=${NOA_BIN:-Noa}
package=.noa-widgets/test-summary
"$widget_noa" +ctl widget validate "$package/widget.json"
for state in normal empty error long; do
  "$widget_noa" +ctl widget validate "$package/widget.json" \
    --data-file "$package/fixtures/$state.json" || exit 1
done

preview_dir=$(mktemp -d)
"$widget_noa" +ctl widget preview "$package/widget.json" \
  --data-file "$package/fixtures/normal.json" \
  --snapshot "$preview_dir/normal.png" --size 960x720
"$widget_noa" +ctl widget preview "$package/widget.json" \
  --data-file "$package/fixtures/long.json" \
  --snapshot "$preview_dir/narrow.png" --size 480x640
```

Fixtures cover a populated result, no tests yet, an unavailable test run, and
long CJK text with multiple failures. They are synthetic and valid against
the same schema. To test interactive scrolling and the preview button, omit
`--snapshot` and open the local preview window.

## Open a live instance

Live instances need Noa's existing control server and the required scopes.
Use the `noa-widgets` skill for the complete event and cleanup loop.

```sh
operation_id=$(python3 -c 'import uuid; print(uuid.uuid4().hex)')
"$widget_noa" +ctl widget open test-summary --repo . \
  --data-file "$package/fixtures/normal.json" --request-id "$operation_id-open"
```

Save the returned `instanceId` and revision. Keep the operation ID for
identical retries; create a new ID for a new operation. Closing the instance
retains its snapshot and pending events. After processing and acknowledging
those events, `widget forget INSTANCE` removes the completed local history.
Application restart restores an inert snapshot; it does not reopen a window
or resume the agent.

Share `widget.json`, this README and the fixtures through Git. Runtime data,
event journals and generated previews are local state and do not belong in
this package. `.noa/` is ignored local storage; `.noa-widgets/` is the tracked
definition registry.
