# Review fixes — 2026-09-05

Baseline: `55ff1a19f05e1720f3557d9923412b13943232fb`.
No new dependencies, persisted formats or configuration keys.

| Review | Change | Regression evidence |
| --- | --- | --- |
| R01 | Canonical temporary-directory AND marker check; validate the opened regular-file descriptor | Either missing condition is rejected; an ordinary temporary file survives a transfer |
| R02 / R06 | Checked shared-memory offset + byte count, validated against actual size; kernel copies instead of userspace mapping access | Real POSIX objects test partial reads and oversized ranges; truncation-after-check coverage on non-macOS Unix |
| R03 | Quota sweep runs after failed placement too | Repeated valid 1×1 uploads with invalid crops remain within an 8-byte quota |
| R04 | Waiter joins completed reader before Exit; bounded drain and cancellation | Deliberately delayed Data precedes Exit; a held slave does not prevent cancellation |
| R05 | One RAII reservation follows input and replies through all queues and the actual write | Capacity exhaustion is nonblocking; write errors and shutdown release queued/in-flight reservations |
| R07 | Request supported image dimensions and validate actual device limit before upload | Real GPU test rejects an oversized replacement without validation errors or stale cache entries |
| R08 / R09 | PNG expansion, transparency and early decoded-size checks | 1/2/4-bit grayscale, RGB/grayscale `tRNS`, indexed alpha; oversized dimensions reject before damaged pixel data is decoded |
| R10 | Image revisions use a process-wide monotonic epoch | Retransmit → delete → recreate and clear → recreate never reuse a previous epoch |
| R11 | Search follows soft-wrap logical lines, retaining byte-to-cell coordinates | Scrollback/live seam, hard newlines, wide/combining text and multi-row highlight assertions |
| R12 | CSI parameter overflow enters the parser's ignore state | Exact limit accepted; extra semicolon/colon requests ignored in scalar and fast scanning paths |
| R13 | Immutable history snapshots, background search, debounce and cancellation | Snapshots survive packing/eviction/live edits; obsolete results are refused; latest query wins |
| R14 | CAS decides the winner for a shared deadline and preserves a losing pane's redraw debt | Different arrival times share one winner; concurrent decisions also have one winner |
| R15 | Index images per draw pass and cache placement uniforms/bind groups | Three real-GPU preparations, including movement, create one resource pair; changed image epoch uploads again |
| R16 | ID/number/age indexes; independent image/frame count limits | Number replacement/removal/eviction stays consistent; tiny images and frames cannot exceed metadata caps |

## Assumptions and operating policy

- PTY output drains for at most two seconds after the direct child exits.
  After this deadline, remaining descendant output can be discarded so pane
  closure cannot wait forever. Production Unix descriptors support cancellation;
  a generic blocking `Read`/`Write` without a poll descriptor cannot be interrupted
  until that operation returns.
- Each pane's writer permits 8 MiB of reserved input and replies, with a 1 KiB
  minimum charge per request. Over-budget requests return `WouldBlock` before
  accepting any prefix. Existing input overflow handling remains nonblocking;
  terminal replies that cannot be accepted are dropped and the failure logged.
- Image metadata is bounded separately at 4,096 images and 16,384 total frames.
  Each decoded/intermediate pixel buffer obeys the configured image byte limit;
  total transient memory may include multiple such buffers. PNG's fixed decoder
  workspace has a 64 KiB minimum even with a smaller image limit.
- Search debounces for 35 ms. One pending query replaces an older query, and
  cancellation is checked between rows. A result whose grid coordinates or live
  contents changed is retried; sustained output can delay publication. Scans are
  still proportional to retained history; live-row incremental search is not
  implemented. The UI and PTY no longer hold the terminal lock for that scan.
- Reverting these changes restores the prior behavior without data migration,
  but would reopen the reported file-deletion and memory-boundary defects.

## Verification

The focused regressions and full workspace suite run on an Apple M4,
arm64 macOS 26.6.2 (25G83). Real GPU and POSIX shared-memory tests run locally.
The final workspace run passed 2,540 tests across 35 suites, with zero failures
and 15 ignored tests. Workspace build, formatting and diff checks passed. The
retained release benchmark was built and executed in both snapshot and
synchronous modes.
Linux truncation runtime behavior and native GUI latency remain unmeasured on
this machine. The Linux-specific regression is gated for Unix targets other
than macOS. Headless performance comparisons are recorded in
[performance-measurements.md](performance-measurements.md#review-follow-up-search-and-image-storage-2026-09-05).

```sh
cargo build --workspace --locked --offline
cargo test --workspace --locked --offline
cargo fmt --all -- --check
git diff --check
```
