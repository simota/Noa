# Spec: Live Wallpaper Feature (live-wallpaper)

## Metadata

- slug: `live-wallpaper`
- title: Live Wallpaper Feature (Live Wallpaper)
- status: `locked`
- owner: simota
- audience: Noa implementers, reviewers, and QA
- reviewers: `TBD`
- recipe: `/nexus spec`
- current phase: `LOCKED`
- as-of: 2026-07-09
- review trigger: implementation starts, `background-image*` semantics change, or animated/media dependencies are added
- related document: `docs/specs/background-image.md`
- build-path decision: `apex`

## Change History

| Date | Phase | Change |
|------|-------|--------|
| 2026-07-09 | FRAME | Captured the problem as a Noa-original directory slideshow extension to static `background-image`. |
| 2026-07-09 | EXPAND | Selected deterministic directory slideshow and deferred shuffle, rescan, playlists, and media formats. |
| 2026-07-09 | CHALLENGE | Added reliability constraints for interval bounds, hidden-window behavior, corrupt PNGs, and multi-surface state. |
| 2026-07-09 | SPECIFY | Added L1 requirements, L2 detail, and L3 acceptance criteria. |
| 2026-07-09 | LOCK | Locked v1 scope and selected `apex` as the implementation path. |
| 2026-07-09 | AMEND | User requested fade-in / fade-out on wallpaper switches; v1 now includes a fixed 2-second cross-fade without adding config. |

## L0 — Vision

### Problem

Noa already supports static PNG terminal backgrounds through the Ghostty-compatible
`background-image*` settings. A user can place one PNG behind the terminal grid, with
fit / position / repeat / opacity applied by the existing renderer. However, changing
that background currently requires choosing a different file and restarting or
reconfiguring the app; Noa has no lightweight way to rotate a set of background
images over time.

The v1 live wallpaper problem is not video playback. It is to let users point the
existing background image setting at a directory of PNG files and have Noa rotate
those images at a controlled interval, while preserving the terminal's primary
qualities: readable text, low input latency, predictable power usage, and safe
behavior when the window is hidden or backgrounded.

### Audience

- Users who already customize Noa's appearance with `background-image`, opacity,
  blur, or themes.
- Power users who want long-running windows, tabs, or quick-terminal sessions to
  feel visually distinct without adding terminal noise.
- Motion-sensitive or focus-sensitive users who need the feature to be opt-in,
  low-frequency, and easy to disable.

### Job To Be Done

When I configure a folder of PNG wallpapers, Noa should periodically rotate the
background image using the same visual semantics as the existing static
`background-image` feature, so my terminal can feel alive or context-specific
without becoming a video player, consuming excessive resources, or reducing text
legibility.

### Success Definition

- `background-image = <file.png>` keeps the current static behavior.
- `background-image = <directory>` enables a PNG slideshow using files in that
  directory.
- Existing `background-image-opacity`, `background-image-fit`,
  `background-image-position`, and `background-image-repeat` apply uniformly to
  every rotated image.
- Rotation happens at a configurable low-frequency interval and stops or throttles
  when surfaces are occluded / backgrounded.
- Missing directories, empty directories, and corrupt PNGs degrade to no image or
  skip the bad image with diagnostics; the terminal continues running.

### Non-Goals For V1

- Video playback, GIF, APNG, WebP, HTML/canvas wallpaper, or shader-based wallpaper.
- Per-frame animation at display refresh rate.
- User-configurable transition effects beyond the fixed short fade used for
  wallpaper switching.
- Per-pane or per-split wallpaper selection.
- Live editing UI in Theme Settings.
- Backgrounds that override the existing text / cursor / selection legibility model.

## Reuse / Constraint Findings

- Existing static background image config and renderer path are already implemented:
  `background-image`, opacity, position, fit, and repeat flow through config,
  `AppConfig`, startup PNG decode, and `BackgroundImageLayer`.
- The renderer has a dedicated full-surface background image layer drawn above
  `LoadOp::Clear` and below pane/cell content, independent of Kitty inline images.
- Static background image decode is startup-only and PNG-only today; live wallpaper
  should extend this path without adding video/media dependencies for v1.
- Existing app redraw scheduling is timer/event-driven through `about_to_wait`;
  slideshow rotation should integrate as another bounded deadline rather than a
  continuous render loop.
- Existing macOS transparency / blur semantics depend on `background-opacity`; image
  alpha must remain controlled by `background-image-opacity`, not by window opacity.
- `background-image` documentation/spec currently describes animated/hot reload as
  deferred, so this spec should be an additive follow-up rather than a replacement.

## Confirmed Direction

The user confirmed the v1 direction:

> Point `background-image` at a directory and display a simple live wallpaper that switches among the `*.png` files inside it at a fixed interval.

## Assumption Ledger

| ID | Assumption | Default chosen | Why | Status |
|----|------------|----------------|-----|--------|
| ASSUME-1 | Live wallpaper v1 is Noa-original additive behavior, not Ghostty parity. | Keep Ghostty-compatible static file behavior unchanged. | Ghostty parity should not regress; directory mode is an extension. | confirmed |
| ASSUME-2 | v1 input is a directory of PNG files only. | No GIF/video/WebP/shader inputs. | Matches existing PNG decoder and avoids new media dependencies. | confirmed |
| ASSUME-3 | Rotation should be low-frequency and opt-in. | Default disabled unless `background-image` points to a directory. | Preserves terminal reliability, battery, and motion safety. | confirmed |
| ASSUME-4 | Directory traversal is non-recursive. | Only direct children matching PNG extension are candidates. | Simpler, deterministic, and avoids surprising filesystem scans. | confirmed |
| ASSUME-5 | Rotation order defaults to filename sort. | Shuffle is deferred. | Deterministic behavior is easier to test and debug. | confirmed |
| ASSUME-6 | Rotation interval should be explicit but low-frequency. | Add `background-image-interval` as positive integer seconds; default `30`; minimum `5`. | Matches existing simple scalar config style while preventing display-rate animation. | confirmed |
| ASSUME-7 | Slideshow state is surface-wide / app-wide, not per-pane. | All background-image surfaces use the same current slideshow image; newly created surfaces receive the current image. | Matches the existing static background-image sharing model and avoids per-pane timing. | confirmed |
| ASSUME-8 | Directory contents are a snapshot, not watched live. | List eligible files at startup / config resolution; no periodic rescan. | Avoids partial-write races, filesystem churn, and watcher platform behavior in v1. | confirmed |

## Candidate Directions

### A. Deterministic Directory Slideshow — selected for v1

- **Config shape**: `background-image = <directory>` enables directory mode.
  `background-image-interval = <integer seconds>` controls the rotation cadence.
- **Runtime behavior**: if `background-image` points to a file, Noa keeps the
  current static PNG behavior. If it points to a directory, Noa lists eligible
  PNG files from that directory and rotates through them at the configured
  interval.
- **Ordering**: deterministic filename order.
- **Visual semantics**: existing `background-image-opacity`,
  `background-image-fit`, `background-image-position`, and
  `background-image-repeat` apply to every image.
- **Power / attention semantics**: rotation is timer-driven, not a continuous
  render loop. Hidden, occluded, or backgrounded surfaces pause or throttle
  rotation and do not catch up missed rotations in a burst.
- **Trade-off**: this is the smallest useful version. It avoids shuffle, live
  directory rescans, manifests, and media/video dependencies, making behavior
  predictable and testable.

### B. Shuffle session order — deferred

Adds a shuffle order. More lively, but weaker reproducibility and more test
surface. Deferred unless deterministic order feels too static after v1.

### C. Directory rescan — deferred

Periodically re-lists the directory so newly added PNGs appear without restart.
Useful, but it introduces filesystem churn, partial-write races, deleted-current
behavior, and more complicated diagnostics.

### D. Playlist / manifest — deferred

Allows curated order via a sidecar playlist. Powerful but beyond the simple
directory-of-PNGs model confirmed for v1.

### E. Exposed power profile — deferred

Would expose preload/cache policy such as `none|next`. The implementation should
still be conservative, but v1 should avoid surfacing renderer-internal policy as
user-facing configuration.

## CHALLENGE Outcome

### Verdict

Conditional Go to `SHAPE`.

The deterministic directory slideshow is the right v1 direction, but it should
not proceed to implementation until the L1 / L3 spec fixes the reliability
contract around directory overloading, interval bounds, hidden-window behavior,
bad images, and multi-surface state. A terminal background update is still a
runtime feature that can affect latency, power, logs, and legibility, so the
implementation contract must stay narrow.

### Must Hold

- `background-image = <file>` keeps the existing static PNG behavior unchanged:
  startup decode, existing visual semantics, and no-image fallback on failure.
- `background-image = <directory>` is a Noa-original extension layered on top
  of the existing Ghostty-compatible static background-image key.
- Candidate enumeration is deterministic and bounded: direct children only,
  PNG extension only, filename sort, empty-directory handling, and permission
  failure behavior are specified.
- Rotation is timer-driven through the existing app deadline model, not a render
  loop.
- Successful switches use a fixed 2-second fade-in / fade-out. The fade is
  bounded to the switch window and does not turn the feature into a continuous
  animation loop.
- Hidden, occluded, or backgrounded surfaces do not produce catch-up bursts.
- Bad PNG files are skipped without stopping the terminal. If no candidate can
  be decoded, the feature degrades to no background image.
- Decode / upload happens on startup or rotation ticks, never per frame.
- Acceptance criteria include static file regression, directory ordering,
  empty/corrupt fallback, and no-catch-up behavior.

### Scope Cuts From CHALLENGE

- Shuffle is deferred because it weakens reproducibility and testability.
- Directory rescan / hot reload / filesystem watchers are deferred because they
  introduce partial-write, deletion, permission, and watcher-platform cases.
- Transitions are limited to a short fixed fade during a successful switch. No
  continuous animation or user-configurable transition system is added.
- GIF, APNG, WebP, video, shader, HTML/canvas wallpaper, and playlist manifests
  remain out of scope for v1.
- Per-pane, per-split, and per-tab wallpaper remain out of scope; v1 follows
  the current surface-level background-image model.
- User-facing preload/cache policy is deferred; v1 should keep cache behavior
  an implementation detail.

### Impact Surface

- `noa-config`: add `background-image-interval` parsing, defaults, merge/apply,
  scalar-key import behavior, and parser tests.
- `bin/noa`: thread the interval into `AppConfig` and extend config-flow tests.
- `noa-app`: split static file decode from directory snapshot resolution,
  maintain slideshow state, add a timer tick, update renderers on rotation,
  run a bounded fade transition, and avoid catch-up after hidden / occluded /
  backgrounded periods.
- `noa-render`: reuse `BackgroundImage`; allow the background-image layer to
  temporarily draw previous/current images during the bounded fade. No new
  media pipeline should be required for v1.
- `docs/specs/background-image.md`: existing locked spec has stale L0 wording
  that says background image support is unimplemented; live wallpaper should
  treat current code as the source of truth and may require a follow-up doc
  correction.

## SHAPE — Proposed V1 Defaults

- `background-image = <file.png>`: existing static behavior.
- `background-image = <directory>`: enable slideshow mode if the resolved path
  is a directory.
- Eligible files: direct child files with ASCII case-insensitive `.png`
  extension; non-PNG content is rejected by PNG decode.
- Directory order: lexicographic filename order.
- Directory snapshot: collected once at startup / config resolution; no live
  rescan in v1.
- Interval key: `background-image-interval = <integer seconds>`.
- Interval default: `30` seconds.
- Interval minimum: `5` seconds. Positive values below `5` clamp to `5`.
  `0`, negative values, and non-integer values diagnose and fall back to the
  default.
- Rotation step: each due tick advances at most one displayable image and
  starts a short fade switch.
- Hidden / occluded / backgrounded behavior: pause or disarm the tick; on
  resume, schedule the next interval from resume time and do not catch up missed
  rotations.
- Multi-surface behavior: the slideshow's current image is app-wide; all
  surfaces using background-image display the same current image, and new
  surfaces receive that current image.
- Failure behavior: unreadable directory, empty directory, or all-corrupt
  candidates degrade to no image; individual corrupt candidates are skipped.

## Considered But Rejected

- Display-refresh-rate animation: rejected because this feature is a slideshow,
  not a render-loop wallpaper engine.
- Shuffle in v1: rejected for deterministic tests and easier bug reports.
- Live directory rescan in v1: rejected to avoid filesystem churn and
  partial-write races.
- User-configurable transition styles or long transition durations: rejected to
  keep v1 as a bounded slideshow rather than an animation engine.
- Recursive directory traversal: rejected to avoid surprising scans, symlink
  loops, and large-tree performance issues.
- Playlist / manifest files: rejected because they add a second configuration
  format beyond the confirmed directory-of-PNGs model.
- Per-pane wallpaper: rejected because the existing feature is surface-level.

## Open Questions / Deferred Decisions

- `background-image-interval` support in Ghostty import should be reviewed at
  implementation time because this is a Noa-specific key, not Ghostty parity.
- Follow-up documentation should correct stale wording in
  `docs/specs/background-image.md` that still describes background-image support
  as unimplemented.

## L1 — Requirements

### Functional Requirements

- **FR-LW-1 — Static compatibility**: `background-image = <file>` MUST preserve
  the existing static PNG behavior, including tilde expansion, one-time decode,
  visual placement semantics, and failure-to-no-image fallback.
- **FR-LW-2 — Directory activation**: `background-image = <directory>` MUST
  enable slideshow mode after the resolved path is identified as a directory.
  Missing paths or unreadable metadata MUST NOT panic.
- **FR-LW-3 — Candidate enumeration**: slideshow mode MUST collect a snapshot
  of direct child entries whose extension is ASCII case-insensitive `.png` and
  whose metadata resolves to a file. It MUST NOT recurse into subdirectories or
  symlinked directories.
- **FR-LW-4 — Deterministic order**: candidates MUST be sorted deterministically
  by path / filename using a stable Rust ordering, independent of filesystem
  iteration order.
- **FR-LW-5 — Interval config**: Noa MUST parse
  `background-image-interval = <integer seconds>`. The default is `30`; positive
  values `1..4` clamp to `5`; `0`, negative, missing-value, and non-integer
  values diagnose and fall back to `30`.
- **FR-LW-6 — Initial image**: directory mode MUST display the first decodable
  candidate in sorted order at startup. If no candidate decodes, Noa MUST launch
  with no background image.
- **FR-LW-7 — Rotation step**: each due rotation tick MUST advance at most one
  displayable image. If the next candidate is corrupt or unreadable, Noa MAY
  scan forward within the snapshot to find the next decodable candidate.
- **FR-LW-8 — No catch-up**: while all relevant surfaces are occluded, hidden,
  or the app is backgrounded, rotation MUST pause or disarm. On resume, Noa MUST
  schedule the next tick from resume time and MUST NOT replay missed intervals.
- **FR-LW-9 — Visual semantics**: every rotated image MUST use the existing
  `background-image-opacity`, `background-image-fit`,
  `background-image-position`, and `background-image-repeat` behavior.
- **FR-LW-10 — Multi-surface state**: slideshow current image MUST be app-wide
  for v1. Existing surfaces and newly created surfaces MUST converge on the same
  current image before they draw while visible.
- **FR-LW-11 — Bounded diagnostics**: missing directories, empty directories,
  permission failures, and corrupt PNGs MUST produce useful diagnostics without
  logging repeatedly every frame or every idle wake.
- **FR-LW-12 — No media expansion**: v1 MUST NOT add dependencies or pipelines
  for GIF, APNG, WebP, video, shaders, HTML/canvas, configurable transitions,
  or filesystem watching.
- **FR-LW-13 — Switch fade**: successful wallpaper switches MUST fade the
  previous image out and the new image in over a fixed 2-second duration. Fade
  progress MUST stop while all relevant surfaces are hidden / occluded or the
  app is backgrounded, and resume behavior MUST NOT replay missed frames.

### Non-Functional Requirements

- **NFR-LW-1 — Idle behavior**: live wallpaper MUST integrate with the existing
  `about_to_wait` deadline aggregation and MUST NOT introduce a continuous
  render loop outside the bounded switch fade.
- **NFR-LW-2 — Decode cadence**: PNG decode and texture upload MUST happen only
  at startup, surface creation, or rotation ticks; fade progress MUST NOT
  re-upload image textures per frame.
- **NFR-LW-3 — Terminal resilience**: any filesystem, decode, or GPU upload
  failure MUST degrade to the previous image or no image, and MUST NOT abort the
  terminal process.
- **NFR-LW-4 — Testability**: parser, candidate enumeration, failure handling,
  timer catch-up behavior, and static-background regression MUST have focused
  automated coverage where practical.
- **NFR-LW-5 — Scope containment**: v1 MUST keep slideshow behavior surface-level
  and config-driven. It MUST NOT add Theme Settings UI, per-pane state, playlist
  files, or user-facing cache policy.

## L2 — Detail

### Configuration Model

- Add a Noa config key: `background-image-interval`.
- Add constants in `noa-config`, for example:
  - `DEFAULT_BACKGROUND_IMAGE_INTERVAL_SECS = 30`
  - `MIN_BACKGROUND_IMAGE_INTERVAL_SECS = 5`
- Store the resolved interval as seconds in `StartupConfig`; map to
  `std::time::Duration` in `noa-app`.
- Thread the interval through `ConfigOverrides`, merge/apply, `bin/noa`
  `app_config_from_startup`, and `AppConfig`.
- Decide during implementation whether the Noa-specific interval key belongs in
  Ghostty import support. It must always be accepted by Noa's native parser.

### Source Resolution

- Resolve `background-image` using the existing app-side path behavior first
  (notably leading `~` expansion).
- If resolved metadata is a file, use the existing static decode path.
- If resolved metadata is a directory, enter slideshow mode.
- If metadata cannot be read, log a warning and disable the background image.
- Directory mode takes a one-time snapshot of candidate paths at startup /
  config resolution. No watcher or periodic rescan runs in v1.

### Candidate Rules

- Only direct children of the configured directory are considered.
- A candidate's extension match is ASCII case-insensitive `.png`.
- A candidate must resolve to file metadata. Symlinked files may be accepted;
  symlinked directories are not traversed.
- Candidate paths are sorted deterministically before decode.
- Empty candidate sets disable the background image and disarm the slideshow
  timer.

### Slideshow Runtime

- Suggested app-level representation:
  - static mode: one optional `BackgroundImage`
  - slideshow mode: snapshot paths, current index, current decoded image,
    interval, next deadline, and a bounded set of already-diagnosed bad paths
- Startup selects the first decodable candidate in sorted order.
- A rotation tick advances from the current index to the next decodable
  candidate, wrapping at the end of the snapshot.
- A successful rotation starts a short fixed fade transition. The renderer may
  temporarily hold the previous and current background textures; per-frame fade
  progress should update alpha coefficients only, not re-decode or re-upload
  the PNG data.
- If a full pass finds no decodable candidate, clear the image, log once for
  the condition, and disarm further slideshow ticks until config reload /
  restart.
- Renderer updates should reuse `Renderer::set_background_image`; no new
  renderer media layer is required.
- Occluded renderers may defer texture upload, but must display the app-wide
  current image before their next visible draw.

### Timer Semantics

- Add a `tick_live_wallpaper`-style operation beside existing timer operations
  in `crates/noa-app/src/app/timers.rs`.
- Include its returned deadline in `about_to_wait`'s single earliest-deadline
  aggregation.
- During the short fade transition, return bounded frame deadlines; after the
  transition completes, return to the normal low-frequency rotation deadline.
- The timer is armed only when slideshow mode has at least one displayable
  candidate and at least one relevant surface is eligible to draw.
- If all relevant surfaces are occluded / hidden, or the app is backgrounded,
  clear or postpone the deadline. On resume, set `next_deadline = now + interval`.
- A tick must request redraw only for surfaces that need the new background
  image.

### Diagnostics

- Use concise warnings for:
  - unreadable configured path
  - configured path that is neither file nor directory
  - unreadable directory
  - empty directory / no eligible PNG candidates
  - corrupt or undecodable PNG candidate
  - all candidates failing decode
- Diagnostics must not include file contents or private image data.
- Repeated corrupt candidates should be logged at most once per snapshot to
  avoid log spam.

### Test Strategy

- `noa-config` unit tests for interval parsing, defaults, merge/apply, and
  native parser recognition.
- `noa-app` unit tests for tilde/file/directory source resolution where
  practical, candidate sorting, extension filtering, corrupt-skip behavior, and
  all-corrupt fallback.
- Timer unit tests for active rotation, no catch-up after backgrounded /
  occluded periods, and one-step advancement.
- Renderer tests should cover transition progress updates in addition to the
  existing static `set_background_image` path.
- Manual GUI verification remains useful for visible multi-window / quick
  terminal behavior.

## L3 — Acceptance Criteria

### Parser And Config

- **AC-LW-1 (FR-LW-1, FR-LW-5)**
  Given `background-image = /tmp/wall.png` and no interval key, when the config
  is parsed and applied, then the existing static background-image fields remain
  unchanged and the interval resolves to `30` seconds.
- **AC-LW-2 (FR-LW-5)**
  Given `background-image-interval = 10`, when the config is parsed, then the
  resolved interval is `10` seconds.
- **AC-LW-3 (FR-LW-5)**
  Given `background-image-interval = 1`, when the config is parsed, then the
  resolved interval is `5` seconds.
- **AC-LW-4 (FR-LW-5)**
  Given `background-image-interval = 0`, `-1`, `1.5`, or `fast`, when the config
  is parsed, then a diagnostic is emitted and the resolved interval falls back
  to `30` seconds.
- **AC-LW-5 (FR-LW-5)**
  Given file config and CLI overrides with different interval values, when they
  are merged, then the existing higher-priority override semantics apply.

### Source And Candidate Selection

- **AC-LW-6 (FR-LW-1, FR-LW-2)**
  Given a configured path that resolves to a file, when startup resolves the
  background source, then Noa uses static mode and does not arm slideshow
  rotation.
- **AC-LW-7 (FR-LW-2, FR-LW-3)**
  Given a configured path that resolves to a directory containing `a.png`,
  `b.PNG`, `notes.txt`, and `nested/c.png`, when candidates are collected, then
  only `a.png` and `b.PNG` are eligible.
- **AC-LW-8 (FR-LW-4)**
  Given eligible files created in arbitrary filesystem order, when candidates
  are collected, then their order is deterministic filename/path sort.
- **AC-LW-9 (FR-LW-6, FR-LW-11)**
  Given an empty directory or a directory with no eligible PNG files, when Noa
  starts, then it logs a diagnostic, displays no background image, and continues
  running.
- **AC-LW-10 (FR-LW-6, FR-LW-7, FR-LW-11)**
  Given a directory where the first sorted PNG is corrupt and the second is
  valid, when Noa starts or rotates, then it logs the corrupt file once, skips
  it, and displays the valid image.
- **AC-LW-11 (FR-LW-6, FR-LW-7, NFR-LW-3)**
  Given every eligible PNG is corrupt or unreadable, when Noa starts or
  completes a full rotation pass, then it displays no background image, disarms
  slideshow rotation, and does not abort.

### Runtime Behavior

- **AC-LW-12 (FR-LW-7, NFR-LW-1, NFR-LW-2)**
  Given slideshow mode with two valid PNGs and interval `5`, when a due tick
  fires while the app is active, then exactly one rotation step occurs and a
  redraw is requested for affected visible surfaces.
- **AC-LW-13 (FR-LW-8)**
  Given slideshow mode is armed, when the app remains backgrounded or all
  relevant surfaces remain occluded for three intervals, then no decode/upload
  burst occurs; on resume, the next deadline is scheduled from resume time.
- **AC-LW-14 (FR-LW-9)**
  Given slideshow mode with configured opacity, fit, position, and repeat, when
  each image is displayed, then the same visual semantics used by static
  background-image are applied.
- **AC-LW-15 (FR-LW-10)**
  Given a second window or quick terminal surface is created after the slideshow
  has rotated, when that surface first draws, then it displays the app-wide
  current image rather than restarting at the first image.
- **AC-LW-16 (FR-LW-12, NFR-LW-5)**
  Given v1 implementation is complete, when dependencies and renderer paths are
  reviewed, then no new GIF/APNG/WebP/video/shader/filesystem-watcher
  dependency or configurable transition pipeline has been added.
- **AC-LW-17 (FR-LW-13, NFR-LW-1, NFR-LW-2)**
  Given a successful directory slideshow rotation, when the wallpaper switches,
  then the previous image fades out and the new image fades in over the bounded
  fixed 2-second transition; per-frame fade progress does not re-upload image
  textures, and the app returns to normal idle waiting after the transition
  completes.

### Traceability Matrix

| Requirement | Acceptance Criteria |
|-------------|---------------------|
| FR-LW-1 | AC-LW-1, AC-LW-6 |
| FR-LW-2 | AC-LW-6, AC-LW-7 |
| FR-LW-3 | AC-LW-7 |
| FR-LW-4 | AC-LW-8 |
| FR-LW-5 | AC-LW-1, AC-LW-2, AC-LW-3, AC-LW-4, AC-LW-5 |
| FR-LW-6 | AC-LW-9, AC-LW-10, AC-LW-11 |
| FR-LW-7 | AC-LW-10, AC-LW-11, AC-LW-12 |
| FR-LW-8 | AC-LW-13 |
| FR-LW-9 | AC-LW-14 |
| FR-LW-10 | AC-LW-15 |
| FR-LW-11 | AC-LW-9, AC-LW-10, AC-LW-11 |
| FR-LW-12 | AC-LW-16 |
| FR-LW-13 | AC-LW-17 |
| NFR-LW-1 | AC-LW-12, AC-LW-13, AC-LW-17 |
| NFR-LW-2 | AC-LW-12, AC-LW-13, AC-LW-17 |
| NFR-LW-3 | AC-LW-9, AC-LW-10, AC-LW-11 |
| NFR-LW-4 | AC-LW-1 through AC-LW-17 |
| NFR-LW-5 | AC-LW-16 |

## Spec Quality Gate

| Dimension | Result |
|-----------|--------|
| Completeness | PASS — confirmed direction, non-goals, requirements, runtime detail, and ACs are present. |
| Unambiguity | PASS — interval defaults, minimums, candidate rules, ordering, and pause semantics are fixed. |
| Verifiability | PASS — each FR/NFR maps to at least one acceptance criterion. |
| Scope coherence | PASS — v1 excludes media formats, configurable transitions, rescan, shuffle, playlists, and per-pane state; only a bounded fixed fade is included. |
| Residual risk | MEDIUM — implementation touches config, app timers, and renderer upload paths. Static background regression tests are mandatory. |
