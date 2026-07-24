# Agent Attention Notification — Specification

## Metadata

- slug: `agent-attention`
- title: Agent notification attention (status rail, tab overview, Dock)
- status: `revised` (2026-07-23)
- owner: simota
- related: [`session-sidebar`](session-sidebar.md) FR-16
- build-path: **feature**

## L0 — Vision

When several Claude Code / Codex / agy sessions run concurrently, Noa should
make a newly raised notification easy to notice without leaving a distracting
animation running. OSC 9/777 indicates that a notification exists; it does not
prove that the process is blocked awaiting a response. The UI therefore uses
the neutral label `通知あり` ("notification") and preserves the notification until the relevant
window gains focus.

- **audience**: developers running multiple concurrent terminal sessions
- **job-to-be-done**: identify which session changed state at a glance
- **success**: the new state gets a brief one-shot emphasis, then remains
  identifiable through a stable shape, color, and label; focus clears it

### Existing foundation

- `SessionCard { unread_bell, attention, busy, process, … }`
- `StatusDot { Blue, Green, Yellow, Red }` with priority
  **attention > bell > busy > idle**
- `SessionDelta::{ Bell, Attention }`; focus clears both flags
- OSC 9/777 posts an OS notification and requests Dock attention
- BEL from a known agent process is promoted to attention; generic BEL remains
  an unread bell

## FRAME — Decisions

- **Persistent representation**: shape + color + categorical status rail;
  no continuously animated state
- **Arrival cue**: one-shot emphasis for `ATTENTION_FLASH_DURATION` (150 ms)
- **Notification scope**: sidebar cards + tab overview + Dock/OS notification
- **Detection triggers**: OSC 9/777 + known-agent BEL
- **Copy**: `通知あり` ("notification"); do not claim “awaiting response” without a dedicated
  response-required protocol

## L1 — Requirements

### Functional

- **FR-A1 Sidebar state rail**: idle uses a hollow green circle and no rail;
  busy uses a blue play icon and a three-segment rail; unread BEL uses a yellow
  bell and a centered short notch; attention uses a red exclamation mark and a
  solid full-height rail. The rail is categorical and never represents percent
  completion.
- **FR-A2 One-shot arrival emphasis**: a card's `false → true` attention
  transition briefly tints its sidebar background and strengthens the Overview
  ring glow. At expiry, one repaint removes the emphasis while the stable red
  indicator, solid rail/ring, and `通知あり` ("notification") label remain.
- **FR-A3 Attention promotion on BEL detection**: known agent processes
  (`ClaudeCode`/`Codex`/`Agy`) promote BEL to `SessionDelta::Attention`.
  Generic or unresolved processes remain `SessionDelta::Bell`.
- **FR-A4 Always-on BEL detection**: BEL detection is independent of sidebar
  visibility. Classification occurs on the main thread.
- **FR-A5 Dock/OS notification**: an unfocused transition to attention requests
  Dock attention once. OSC 9/777 also posts to Notification Center; BEL-promoted
  attention does not, avoiding notification overload.
- **FR-A6 Clearing**: focusing the relevant window immediately clears attention,
  unread BEL, and any unfinished one-shot emphasis.
- **FR-A7 Repeated firing**: another attention delta while attention is already
  pending does not restart the emphasis. A new occurrence after focus cleared
  the state starts a fresh emphasis.

### Non-Functional

- **NFR-A1 No draw-path locking**: state is read from App/SessionStore data and
  rendering never locks `Terminal`.
- **NFR-A2 Bounded redraw**: attention adds only the transition repaint and one
  expiry repaint; no periodic animation timer remains.
- **NFR-A3 Redundant encoding**: shape and rail geometry supplement color so
  the four states are not distinguished by color alone.
- **NFR-A4 False-positive suppression**: only known agent processes promote BEL
  to attention; unresolved processes use the safer unread-bell state.

## L3 — Acceptance Criteria

- **AC-A1 (FR-A1/NFR-A3)**: unit tests verify distinct indicator glyphs/colors,
  rail precedence, and the three rail geometries.
- **AC-A2 (FR-A3/NFR-A4)**: unit tests verify known-agent BEL promotion and
  generic/unresolved fallback.
- **AC-A3 (FR-A6)**: store/unit verification confirms window focus clears
  attention and unread BEL; manual verification confirms the visual clears.
- **AC-A4 (FR-A7)**: repeated attention while pending leaves the existing
  emphasis deadline unchanged.
- **AC-A5 (FR-A2) [manual]**: a new notification briefly emphasizes the sidebar
  card and Overview ring once, then settles without blinking.
- **AC-A6 (FR-A5) [manual]**: OSC 9/777 requests Dock attention and posts an OS
  notification; BEL-promoted attention requests Dock attention only.
- **AC-A7 (NFR-A2)**: after the 150 ms expiry repaint, the event loop returns to
  its normal idle wait and has no attention-specific periodic wake-up.

## Implementation Notes

1. `App::attention_flash_until` stores per-card expiry deadlines outside the
   GUI-agnostic SessionStore.
2. `apply_session_delta` inserts a deadline only on `false → true` attention.
3. `tick_transient_overlays` removes expired entries, redraws sidebars, and
   invalidates affected Overview tiles once.
4. Pane moves rekey the transient deadline so the remaining emphasis follows
   the card without restarting.

## Open Questions

- Should a future protocol expose a distinct “response required” state and
  permit stronger copy than `通知あり` ("notification")?
- Should the 150 ms duration become configurable if Noa later adds a global
  reduced-motion/animation preference?
