# Agent Attention Notification — Specification

## Metadata
- slug: `agent-attention`
- title: Agent response-wait notification (sidebar blink, tab overview, Dock)
- status: `locked` (2026-07-05)
- owner: simota
- related: [`session-sidebar`](session-sidebar.md) FR-16 (this spec extends and details FR-16; it is the upper-level document)
- build-path: **feature** (a differential implementation on top of the existing FR-16 foundation. Design → implementation → AC verification. [manual] visual ACs are verified by hand)

## L0 — Vision
When running Claude Code / Codex / agy across multiple concurrent sessions, it's easy to miss that a given session has stopped, waiting for user input or a decision, which stalls work. Noa already implements FR-16: it receives OSC 9/777 notifications and shows a static red mark (`· awaiting response`) on unfocused session cards, plus a `●` in the tab overview. But (1) because it's static, it's easy to overlook, and (2) the only detection path is OSC 9/777, so agents that signal waiting via bell (BEL) are not caught. This spec defines **active attention-grabbing via blinking** and **the addition of BEL detection**.

- **audience**: developers running multiple agent sessions concurrently (i.e., the author)
- **job-to-be-done**: notice immediately which session is awaiting a response, even without watching the terminal
- **success**: the moment an agent requests interaction, the relevant session blinks in the sidebar and tab overview, converges to a static mark after a few seconds, and the Dock bounces once. Everything clears on focus.

### Existing foundation (already implemented in this session — to be reused)
- `session_store.rs`: `SessionCard { unread_bell, attention, busy, process, … }`, `StatusDot { Blue, Green, Yellow, Red }`, `status_dot()` priority **attention > bell > busy > idle**
- `SessionDelta::{ Bell, Attention }` — `apply` sets the flags, `Upsert` preserves them, `clear_bell_for_window` clears both on focus
- `io_thread.rs`: `sidebar_bell = sidebar_visible && term.take_pending_bell()` → `SessionDelta::Bell` / `pending_notifications` (OSC 9/777) → `UserEvent::Notify`
- `event_loop.rs`: `Notify` → under the `should_notify` gate, `post_notification` + `apply_session_delta(Attention)` (unfocused windows only)
- `notification.rs`: `post_notification` posts to the OS Notification Center + `request_dock_attention()` (`NSInformationalRequest`, a single bounce)
- `overview.rs`: `overview_tile_label` prefixes `●` when `attention || unread_bell`
- `app/sidebar.rs`: `process_badge` + appends `· awaiting response` with `SIDEBAR_DOT_RED` on attention / `classify_agent(process) → AgentKind`
- **Animation timer foundation**: `cursor_blink_visible` / `cursor_blink_deadline` / `tick_cursor_blink` + `about_to_wait`'s `WaitUntil` wake mechanism (snaps to `true` on input). Blinking rides on this same single timer source.

### Hard constraints (inherited from the session-sidebar spec)
1. The renderer/draw path never locks `Terminal` (via the publish slot, NFR-1).
2. `session_store.rs` / `sidebar.rs` remain GUI-agnostic (no winit/wgpu, NFR-6). The blink "phase calculation" must be a pure function so it's unit-testable.
3. All sidebar characters are cell-rendered. Marks reuse the existing dot/label primitives (no new shader needed).
4. `Instant`/wall-clock time is owned by the main thread (App). `session_store` only holds `WallClock`; the monotonic clock is handled on the App side.

## FRAME — Decisions (AskUserQuestion 2026-07-05)
- **Visual representation**: blink → static after a few seconds (`blink → static after a few seconds`). Blink briefly right after notification to grab attention, then converge to a static mark.
- **Notification scope**: sidebar cards + tab overview + Dock bounce/OS notification (3 channels)
- **Detection triggers**: OSC 9/777 notification (already implemented) + bell (BEL)

## L1 — Requirements

### Functional
- **FR-A1 Blink → static convergence (sidebar)**: starting from the moment a session card's attention mark (red dot + `· awaiting response` label) transitions from `false` to `true`, it toggles visible/invisible at **`ATTENTION_BLINK_HZ`** (default 1.5 Hz) for **`ATTENTION_BLINK_DURATION`** (default 6 seconds), then converges to a static visible (red) state. After convergence, attention persists until focus (per FR-16). Only the dot and label blink — other card elements and text remain always visible.
- **FR-A2 Blink → static convergence (tab overview)**: the `●` prefix on the title band also blinks → converges to static using the same phase and parameters as FR-A1. It only redraws while the overview is displayed (riding on the existing due-tile mechanism).
- **FR-A3 Attention promotion on BEL detection**: if a session's foreground process is a known agent (`classify_agent` returns `ClaudeCode`/`Codex`/`Agy`), that session's BEL (`take_pending_bell`) is promoted to **`SessionDelta::Attention`**. BEL from a `Generic` foreground process still stays as `SessionDelta::Bell` (yellow dot, unread bell) as before — to avoid misinterpreting a generic program's bell as an "awaiting response" signal.
- **FR-A4 Always-on BEL detection**: BEL is **always drained and sent from the io thread regardless of sidebar visibility** (previously `sidebar_bell` was gated on `sidebar_visible`). Classification happens on the main thread, which has the foreground process (the io thread doesn't know the process) — known agents are promoted to attention (always reflected, flowing through the Dock/overview channels), while `Generic` sets `unread_bell` (the flag is only rendered while the sidebar is visible, and clears on focus). Implementation consequence: a generic bell that rings while the sidebar is hidden is no longer "shown later when reopened" but rather "rendered only while visible" (a minor spec deviation, accepted).
- **FR-A5 Dock/OS notification**: on transition to attention, if the window is unfocused, bounce the Dock once (`request_dock_attention`). Attention arriving via OSC 9/777 also posts to the OS Notification Center as before. **Attention promoted via BEL only bounces the Dock** and does not post to the OS Notification Center (bells are frequent, so this avoids notification overload).
- **FR-A6 Clearing**: attention/blink state clears when the relevant window gains focus (`clear_bell_for_window` clears both attention/unread_bell, per FR-16). It clears immediately even mid-blink. A focused window never sets attention (per FR-16).
- **FR-A7 Handling repeated firing**: if another attention delta arrives for a card that is already in attention (blinking or already converged), **do not restart the blink phase** (don't re-blink after convergence). However, a new occurrence after being cleared by focus starts a fresh blink.

### Non-Functional
- **NFR-A1 No draw-path locking**: blink visibility is computed from the App-side monotonic clock and never locks `Terminal`.
- **NFR-A2 Bounded redraw**: blinking always stops after `ATTENTION_BLINK_DURATION`, and no redraw is requested after convergence (returns to idle). The blink wake source is integrated into the same single `WaitUntil` timer as cursor-blink; no duplicate timer is created. If there are no attention cards, the blink timer is disarmed.
- **NFR-A3 Pure and testable**: blink phase computation (`elapsed → visible: bool` / convergence check) lives as a pure function independent of winit/wgpu in `sidebar.rs` (or a pure module), and is unit tested.
- **NFR-A4 False-positive suppression**: BEL promotion is limited to cases where the foreground process classification is a known agent (FR-A3). Classification does not promote when `process` is unresolved (non-macOS / not yet polled) (safe default = the existing yellow bell).

## L3 — Acceptance Criteria

- **AC-A1 (FR-A1)**: unit test that the pure blink-phase function `attention_blink_visible(elapsed, duration, hz)` returns (a) visible near `elapsed=0`, (b) invisible after half a period, (c) always visible (converged) once `elapsed >= duration`.
- **AC-A2 (FR-A3/NFR-A4)**: unit test that the BEL-promotion pure function returns `Attention` for foreground processes `claude`/`codex`/`agy`/`gemini`, and `Bell` (not promoted) for `zsh`/`cargo`/`node` and `process=None`.
- **AC-A3 (FR-A6)**: verify via unit test (store) + [manual] that when a window with an attention card (including mid-blink) gains focus, `attention=false` and blinking stops.
- **AC-A4 (FR-A7)**: unit test that applying another `Attention` delta to an already-converged attention card does not update the blink-phase origin (the origin timestamp is unchanged).
- **AC-A5 (FR-A2) [manual]**: visually confirm on a real device that when an unfocused agent session starts awaiting a response, the sidebar card and the tab overview `●` blink in the same phase, then converge to static red after about 6 seconds.
- **AC-A6 (FR-A5) [manual]**: visually confirm that the Dock bounces once on the transition to awaiting-response, that OSC 9/777-driven attention also posts to the OS Notification Center, and that BEL-promoted attention does not.
- **AC-A7 (NFR-A2)**: confirm via [manual] + logs that after convergence the app returns to idle (`ControlFlow::Wait`) and that the blink timer is disarmed when there are no attention cards.

## Implementation Sketch (design notes — to be finalized during the implementation phase)
1. **Recording the attention onset**: the App side holds `attention_onset: HashMap<SessionCardId, Instant>` (`session_store` doesn't hold an `Instant` since it must stay GUI-agnostic). When applying an `Attention` delta in `apply_session_delta`, insert only when the card transitions `false → true` (FR-A7). Remove the relevant window's entries in `clear_session_bell_for_window`.
2. **Blink timer integration**: merge `attention_blink_deadline` into `about_to_wait`'s `WaitUntil`, the same way as `cursor_blink_deadline`. Arming condition = at least one attention card is still blinking (`elapsed < duration`) in a visible sidebar/overview.
3. **Drawing**: suppress (hide) the dot/`· awaiting response` drawing in `app/sidebar.rs` and the `●` marker in `overview.rs` while `attention_blink_visible` is `false`. Phase computation stays a pure function.
4. **BEL promotion**: extend the `sidebar_bell` check in `io_thread.rs` — if the foreground process classification is a known agent, emit `SessionDelta::Attention` (no gate, FR-A4); otherwise emit the existing `SessionDelta::Bell` (keeping the sidebar-visibility gate). The source `process` classification comes from the existing session metadata worker.
5. **Dock branching**: attention promoted via BEL only bounces the Dock (skip posting to the OS Notification Center). OSC 9/777-driven attention is unchanged.

## Open Questions
- **OQ-1 Blink parameters**: should `ATTENTION_BLINK_DURATION` (6s) / `ATTENTION_BLINK_HZ` (1.5Hz) become config keys, or stay compile-time constants (⚠G precedent)? The first version recommends constants.
- **OQ-2 Scope of BEL promotion**: is the known-agent criterion sufficient, or should a heuristic like "foreground process is not a shell and output has been idle for a while" (rejected in AskUserQuestion) become an optional feature later?
- **OQ-3 Post-convergence persistence**: should the static red after convergence remain completely still until the next focus, or have a very low-frequency "breathing" pulse (the first version keeps it completely still, matching the current FR-16 behavior)?
