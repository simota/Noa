# Spec: Agent CLI Auto-Approve Mode (auto-approve-mode)

- slug: `auto-approve-mode`
- status: `locked` (signed off 2026-07-08)
- owner: simota
- build-path decision: **apex** (`/nexus apex` — live AC: T-1 signature capture and AC-11/12/13 GUI visual checks remain manual)

## L0 — Vision

- **Problem:** When running Claude Code / Codex / agy inside a noa tab, every tool execution stops at an approval prompt (y/n, numbered menu, Enter confirmation), and the agent keeps waiting until the user comes back to the terminal and types. With multiple tabs running in parallel, waiting for approvals dominates throughput.
- **Target:** Users running multiple AI agent CLIs in parallel on noa (i.e., simota's own workflow).
- **Job:** For opt-in tabs only, noa detects approval prompts from recognized agent CLIs and automatically sends an affirmative response, enabling unattended operation.
- **Definition of success:** In tabs with auto-approve ON, agents never stall waiting for approval. Zero unintended keystrokes from false positives.
- **Biggest risk:** Auto-approving a destructive operation due to a false positive. Detection accuracy and gate design are the core of this spec.

### FRAME decisions (user answers, 2026-07-08)

- Approach axis: **on-screen detection + synthetic keystrokes** (not CLI flag injection)
- Granularity: **per-tab opt-in**
- Safety guards (all adopted): approve only known patterns / never approve dangerous operations / pause on user keystroke / mandatory visual indicator

### Reusable assets & constraints (Lens reuse-scan)

| Asset | Location | Use |
|------|------|------|
| Agent detection `classify_agent` → `AgentKind{ClaudeCode,Codex,Agy,Generic}` | `crates/noa-app/src/sidebar.rs:734` | Gate for auto-approve (recognized agents only) |
| Foreground process probe (1s poll) | `crates/noa-pty/src/pty.rs:213,269`, `branch_poll.rs:44,283` | Determines agent presence |
| Tail-of-screen N-row read pattern `preview_rows` | `crates/noa-app/src/io_thread.rs:315` | Template for prompt-detection scanning |
| pty synthetic keystrokes `write_pane_pty_bytes` → `PtyInputQueue` | `crates/noa-app/src/app/input_ops/terminal.rs:162`, `io_thread.rs:200` | Injection path for e.g. "y\r" (no key-encoding needed) |
| Attention state (OSC 9/777 notifications, Bell escalation) | `app/event_loop.rs:96-121`, `sidebar.rs:775` | Existing "agent awaiting response" signal — a candidate trigger |
| Locked section + throttle in the io_thread feed | `io_thread.rs:395,426` | Where post-output scanning can piggyback (zero extra lock acquisitions) |
| OSC 133 shell marks / `has_running_program` | `crates/noa-grid/src/terminal.rs:63,345,467` | Determines whether a CLI is running (though prompts internal to the CLI are invisible) |
| Config/command infrastructure (Config bool / AppCommand / palette / keybind) | `app/config.rs`, `app/commands.rs:23`, `command_palette.rs:103` | Where the toggle gets implemented. Per-tab needs a new flag on `Surface` |

**Threading constraint:** `Terminal` is `Arc<Mutex>` (parking_lot). A two-stage design — detection in io_thread (inside the existing lock) and keystroke injection via UserEvent on the main thread's `write_pane_pty_bytes` — fits the existing architecture. Synthetic keystrokes share `PTY_INPUT_OVERFLOW_BYTE_CAP` with real user input. CLIs running behind a node wrapper have a known limitation of falling into `Generic` (`sidebar.rs:731`).

## Candidate options (EXPAND)

| Option | Trigger | Detector | Response | Danger judgment | Scale |
|----|--------|--------|------|----------|------|
| A: Attention-Gated Minimal | Attention rising edge only | Per-CLI hardcoded signatures | Always the first affirmative choice | Static keyword | S |
| B: Debounced Feed-Scan + Regex Table | Every feed, debounced | Configurable regex table (extensible) | Response map per type | Command extraction + classification | L |
| C: 1s-Poll Matrix | Piggybacks on branch_poll's 1s tick | AgentKind × prompt-type matrix | Prioritize don't-ask-again | Rate-limits approval count | M |
| D: Hybrid State-Machine | Attention edge → burst scan | Two-layer matrix + regex | Extraction classification + rate limit | Response map per type | L |

Riff assessment: minimal risk for v1 = C, assuming future extension = D. Exposing config regex in B is excessive for v1.

### Flux insights (safety mechanisms to fold into the design)

- Match only after the line is settled and stable for a few frames (~120ms), and only when the cursor is on the prompt line (guards against iTerm2/partial-render artifacts)
- After a keystroke, debounce on the same signature plus a consumed flag; don't rearm until the screen changes (guards against double-firing)
- No next shot until the screen confirms it "advanced" after a keystroke (guards against blind-firing in tmux)
- Require alt-screen flag + viewport-at-bottom as preconditions (guards against scroll-out)
- Auto-disable + notify on unrecognized version text (fail-safe)
- Suppress firing while IME is active, during paste, or right after recent user input
- Contrarian view: scope the terminal-side value to "uniform policy across all CLIs, per-tab visibility, and auditing" / consider defaulting to auto-escalation rather than auto-yes / consider having CLIs emit a structured OSC channel standard (the VS Code approach)

## Selection and rejected options (CHALLENGE)

**Selected (user confirmed 2026-07-08): Option C (1s-Poll Matrix) + Flux safety mechanisms**

- Response policy: always the first affirmative choice (e.g. "1. Yes" for Claude Code). Don't-ask-again-style responses are not chosen, since they pollute the CLI's own config.
- Structured OSC channel: parked as an Open Question. Only guarantee that the detector is trait-ized so it can be swapped later.

### CHALLENGE revisions (Omen/Ripple review, 2026-07-08)

**Trigger-layer swap (conditional GO):** Piggybacking on branch_poll doesn't actually work in the real code — the worker doesn't hold `Arc<Mutex<Terminal>>` (branch_poll.rs:220-243), the per-tab flag (owned by `Surface` on the main thread) isn't visible there, and window/pane resolution isn't possible. Therefore:
1. **Move detection into `feed_terminal_batch` in io_thread** (lock already held, pane already known, natural throttling, near io_thread.rs:395). Scanning uses two-consecutive-match debouncing to block false matches from partial rendering (RPN80).
2. **Injection uses a new `UserEvent::AutoApprove{..}`** → main thread resolves card→(window,pane) authoritatively → existing `write_pane_pty_bytes`. `SessionDelta` is store-apply-only and can't be reused for this.
3. **Invert to an allowlist approach**: approve only known-benign prompt signatures. After approving, lock the matched region's cell-hash and cap consecutive approvals to block re-arming loops (RPN45).

**Void scope compression — v1 safety mechanisms are 6 systems plus 1 addition:**
Signature matching (fail-safe against unknown text is built in as a non-fire) / tab badge / consumed flag (subsumes cell-hash and screen-advance confirmation) / armed only during alt-screen or while tracking the viewport tail / suppression during IME/paste/recent user input (3s) / rolling 6-in-window auto-OFF + attention. Addition = **audit log of auto-approvals** (ring buffer, most recent N entries).
CUT: full-text dangerous-word parser (replaced by limiting signatures to benign types) / 120ms line-stability wait (replaced by two-consecutive-match even under feed-driven polling) / trait-izing the detector (YAGNI).

**Top remaining risks per Omen:** arrow-key UI variants lack numbers (RPN60) → mitigated with a dual signature of number + highlight position, mismatch = no-fire / wrong target pane for the keystroke (RPN30) → rescan the target immediately before injection / version differences in "1" vs "1\r" → fix the injected byte sequence per agent × signature in a table, verified on real hardware.

**Rejected:**
- Option A (Attention-Gated Minimal) — can't tolerate missing prompts that never trigger attention
- Option B (Regex Table) — exposing config regex is excessive for v1, risk of misfires, design debt
- Option D (Hybrid State-Machine) — L-level implementation/verification cost. Safety-layer elements (armed/cooldown, dual gating) are selectively folded into C
- CLI flag injection approach — rejected in FRAME (requires hooking into the launch path)
- Prioritizing don't-ask-again responses — leaves a side effect in the CLI's allowlist across sessions

## Proposal (SHAPE)

### Solution (Modified C)

A per-tab opt-in "auto-approve mode." In tabs where it's ON, within io_thread's feed processing (right after output, with the Terminal lock already held), the visible viewport is scanned; only when it matches a recognized agent (ClaudeCode/Codex/Agy) × a known **benign prompt signature** (Edit/Write/Read approval, Enter confirmation) does the main thread inject a fixed byte sequence (e.g. "1") into the pane's pty via `UserEvent::AutoApprove`. Nothing happens for unrecognized text, Bash approvals, or unrecognized agents (fail-safe).

### In-scope (v1)

1. Per-tab toggle: `Surface` flag + `AppCommand::ToggleAutoApprove` + palette entry + keybind (global default via config, default OFF)
2. Detector: hardcoded signature matrix of AgentKind × prompt type. Dual signature of number + highlight position. Confirmed on two consecutive feed matches
3. Precondition gate: armed only during alt-screen or while tracking the viewport tail. The cursor-row condition is folded into the signature
4. Injection: `UserEvent::AutoApprove{card,bytes}` → authoritative resolution on the main thread (reconfirm the target right before injecting) → `write_pane_pty_bytes`. Fixed injection byte sequence table per agent × signature
5. Consumed flag: don't rearm until the cell-hash of the matched region changes after approval + cap on consecutive approvals
6. Input-conflict suppression: no-fire during IME preedit, during paste, or right after recent user input (3s)
7. Runaway breaker: auto-OFF + attention notification after 6 approvals within a rolling 60s window
8. Visualization: tab/sidebar badge (mode ON) + flash on fire
9. Audit log: record of the most recent N auto-approvals (ring buffer + display surface)

### Out-of-scope

- Automating Bash command execution approval (v2 candidate: only leave design room for a ~20-line denylist extension)
- Config-file regex table / user-defined patterns
- CLI flag injection (e.g. `--dangerously-skip-permissions`) approach
- Structured OSC channel (parked)
- Improving detection for agents that fall into `Generic` due to node wrappers, etc.
- Full-text dangerous-word parser

### Assumptions

- Approval prompt text for target CLIs is stable within known version ranges (if it changes, it simply fails to fire — safe by default)
- Claude Code's numbered menu accepts numeric keystrokes (real-hardware verification is included in SPECIFY's AC)
- The initial set of detection signatures is built from real prompt capture (Claude Code first, Codex/agy as they're captured)

### SHAPE decisions (user answers, 2026-07-08)

- **Fires even on the focused tab.** Collision avoidance is left to recent-user-input suppression (3s) + IME/paste guards
- The canonical in-scope list is the 10 items under the "## Scope" section (the 9 items in this SHAPE section plus firing while focused)
- Audit log display surface: **inside the sidebar card** (recent approval count/latest item; a dedicated modal is deferred to v2)

## L1 — Requirements

### Functional Requirements (FR)

- **FR-1** Per-tab opt-in toggle: auto-approve mode can be turned ON/OFF per `Surface` (`AppCommand::ToggleAutoApprove`, command palette entry, keybind, `config` default value). Default is OFF.
- **FR-2** Agent gate: armed only on panes where `classify_agent` returns `ClaudeCode`/`Codex`/`Agy`. `Generic`/unrecognized never fires.
- **FR-3** Prompt detection: match the visible viewport against a **hardcoded signature matrix** of `AgentKind` × prompt type. Dual signature = ① anchor text + numeric label, ② **selection-marker condition** (the selection cursor character, e.g. "❯", must be at the start of the line for the first affirmative option; grid character-based check, independent of SGR attributes). If either fails to match, no fire.
- **FR-4** Two-consecutive-match debounce: confirm only when the same signature matches on two consecutive feed scans in a row (blocks false matches from partial rendering).
- **FR-5** Precondition gate: armed only during alt-screen or while tracking the viewport tail (**scrollback display offset == 0**, i.e. the live tail is being displayed).
- **FR-6** Affirmative response injection: on confirmation, the main thread authoritatively resolves (window,pane) via `UserEvent::AutoApprove` and sends the fixed injection byte sequence for that agent × signature via `write_pane_pty_bytes`.
- **FR-7** Pre-injection reconfirmation: right before injecting, the main thread rescans the target pane; if the signature has disappeared, abort the send (guards against wrong target pane / stale state).
- **FR-8** Consumed flag: after approval, don't rearm until the **cell-content hash of the signature-matched row range (anchor row through the last option row)** changes, plus a cap on consecutive approvals.
- **FR-9** Input-conflict suppression: no-fire during IME preedit, during paste, or within 3s of recent user input.
- **FR-10** Runaway breaker: auto-OFF + attention notification after M approvals (default 6) within a rolling 60s window.
- **FR-11** Non-approval of dangerous operations: Bash execution approvals and unknown text are never included in the matrix signatures, and always fail to fire (fail-safe).
- **FR-12** Visualization: badge on the tab/sidebar card while mode is ON, flash on fire.
- **FR-13** Audit log: record the most recent N (default 16) auto-approvals in a ring buffer, and show the recent approval count/latest item inside the sidebar card.

### Non-Functional Requirements (NFR)

- **NFR-1** Performance: the detection scan piggybacks on the already-held lock inside `feed_terminal_batch` (`io_thread.rs:395`), with zero extra Terminal lock acquisitions. Scan range is limited to the visible viewport row count, keeping the per-feed extra cost within O(rows×cols), comparable to `preview_rows`.
- **NFR-2** Fail-safe principle: unknown means no-fire. Signature mismatch, unrecognized version text, or unmet precondition gate all consistently resolve to "do nothing."
- **NFR-3** Thread safety: detection happens in io_thread (inside the lock, pane already known); injection is a two-stage flow via `UserEvent::AutoApprove` → main thread. Synthetic keystrokes share `PTY_INPUT_OVERFLOW_BYTE_CAP` with user input.
- **NFR-4** No CLI-side pollution: don't-ask-again-style responses are never chosen; always send only the first affirmative choice (leaves no side effect in the CLI's own allowlist).
- **NFR-5** Room for extension: the signature matrix is structured to allow later additions such as a Bash denylist, but v1 exposes no public config and no trait-ization (YAGNI).

## L2 — Detail

### Detector (pure-function core + io_thread glue)

- **Pure-function seam (test boundary)**: the detection core is factored out as a side-effect-free pure function —
  `detect(viewport_rows: &[RowText], cursor: CursorPos, agent: AgentKind, now: Timestamp, state: &AutoApproveState) -> Decision` (`Decision = Fire{signature_id, bytes} | Hold | Suppressed{reason}`). Time, the recent-user-input timestamp, and IME/paste state are all injected as arguments, and AC-2..9 are verified as unit tests of this pure function. No io_thread/pty/GUI required.
- Placement (glue): inside `feed_terminal_batch`'s lock section, alongside the `preview_rows` scan. It just extracts the viewport row text and calls `detect`. The pane is already known, no extra lock acquisition.
- Scan range: the visible viewport (only during alt-screen or when scrollback display offset == 0).
- Signature matrix structure: `AgentKind × PromptKind → Signature`. A `Signature` is a dual signature of { anchor text, numeric label ("1", etc.), selection-marker condition (character-based check that "❯" etc. is at the start of the first affirmative option's line) } plus an injection byte sequence. Initial `PromptKind` set (Claude Code): Edit approval / Write approval / Read approval / AskUserQuestion selection / Enter confirmation. Actual text and byte sequences are finalized in the prerequisite task T-1 (signature capture).
- Debounce state: per-pane state holding "last matched signature + match count," confirmed on two consecutive matches.

### State machine (per-pane `AutoApproveState`)

- Fields: `armed` (precondition gate satisfied) / `awaiting_change` (post-approval, waiting for cell-hash change) / `cooldown`, `cell_hash` of the last approved region, rolling-window counter (list of approval timestamps within the 60s window), timestamp of the last user input.
- Transitions: `armed` → two consecutive signature matches → `UserEvent` fires → `awaiting_change`. Rearms to `armed` when the hash of the signature-matched row range changes. Exceeding 6 within the 60s window transitions to `disabled` (auto-OFF) + attention.

### Injection path

- `UserEvent::AutoApprove { card_id, bytes }` (added to the UserEvent enum in `events.rs`, following existing variants).
- On the main thread, `card_id` is authoritatively resolved to (window, pane) → **the target pane is rescanned right before injection (FR-7)** → only if the match still holds is the byte sequence passed to `write_pane_pty_bytes` (`app/input_ops/terminal.rs`) and queued into `PtyInputQueue`.
- Evaluation of suppression conditions (IME/paste/recent input/window exceeded): evaluated **both** at detection time (io_thread, early rejection) and right before injection (main thread, authoritative decision).

### Toggle / config

- New `auto_approve: bool` flag on `Surface` (`app/state.rs:511`).
- `AppCommand::ToggleAutoApprove` (registered with menu ID/title/palette following existing patterns like `ToggleSidebar`).
- Entry added to `command_palette_entries()` (`command_palette.rs`).
- Global default `auto_approve: bool` in `config.rs` (following the naming convention of existing keys like `sidebar_enabled`/`visual_bell`, default `false`). Keybind is set via config.

### Visualization

- Badge: mode-ON badge on the tab title and sidebar card (near `sidebar.rs`'s `CardLines`/process badge row).
- Fire flash: brief highlight on the card/tab when an approval is sent.
- Audit log: per-pane ring buffer (capacity 16 entries, holding {timestamp, agent, PromptKind}). Displays "Auto-approved: N / Latest: <PromptKind>" inside `SessionCard`. A dedicated modal is deferred to v2.

## L3 — Acceptance Criteria

Prerequisite: AC-2..9 are verified as unit tests of the pure detection function `detect(...) -> Decision` (see L2), requiring no io_thread/pty/GUI.

- **AC-1 (FR-1)** Unit: toggling `Surface.auto_approve` flips ON/OFF, default OFF. The item appears in the palette.
- **AC-2 (FR-2)** Unit: with `agent=Generic`, `detect` never returns `Fire` even when given screen text matching a known signature.
- **AC-3 (FR-3, FR-11)** Unit: for text outside the matrix (Bash approval prompt, or any unknown text), `detect` never returns `Fire`.
- **AC-4 (FR-3)** Unit: if the selection marker "❯" is not at the start of the line for the first affirmative option (pointing at a different option, or absent), no fire even if the anchor text matches.
- **AC-5 (FR-4)** Unit: if the signature matches only on one scan (and disappears on the next), it is not confirmed and no fire occurs. `Fire` occurs on two consecutive matches.
- **AC-6 (FR-5)** Unit: `detect` returns `Suppressed` when outside alt-screen and scrollback display offset > 0.
- **AC-7 (FR-6, FR-7)** Unit: after firing, if the pre-injection rescan (on the main thread side) finds the signature gone, `write_pane_pty_bytes` is not called.
- **AC-8 (FR-8)** Unit: immediately after approval, no refire occurs while the same signature remains and the cell-content hash of the signature-matched row range is unchanged. Rearms after the hash changes.
- **AC-9 (FR-9)** Unit: `detect` returns `Suppressed` under each of these argument conditions: IME preedit active / paste in progress / recent-user-input timestamp within 3s of now.
- **AC-10 (FR-10)** Unit: after 6 approvals within a 60s window, the mode auto-disables and an attention notification is raised.
- **AC-11 (FR-12, FR-13)** Live GUI: with mode ON, badges appear on the tab/sidebar, a flash occurs on fire, and the card's approval count/latest item update. The audit ring buffer discards the oldest entry beyond 16 (also covered by unit tests).
- **AC-12 (FR-3, FR-6)** Live GUI: on a tab with mode ON, each Claude Code PromptKind (Edit/Write/Read/AskUserQuestion/Enter confirmation) is automatically approved, visually confirmed to let the agent proceed without stalling. **Requires prerequisite task T-1 to be complete.**
- **AC-13 (FR-6)** Live GUI (version-difference check): confirm the injection byte sequence for each agent × signature (e.g. "1" vs "1\r") is accepted on real hardware, and lock it into the table.

### Prerequisite task (implementation phase, precedes the ACs)

- **T-1 Signature capture**: capture real approval prompts on real hardware for Claude Code (first), Codex, and agy, and finalize the signature matrix (anchor text, numeric label, selection marker, injection byte sequence). Any agent × PromptKind not yet captured is left out of the matrix (i.e. remains a no-fire).

## Scope

### In-scope (v1)

1. Per-tab opt-in toggle (`Surface` flag + `AppCommand::ToggleAutoApprove` + palette + keybind + config default OFF)
2. Hardcoded signature matrix of `AgentKind` × prompt type (dual signature of number + selection-marker condition, confirmed on two consecutive feed matches)
3. Precondition gate (armed only during alt-screen or while tracking the viewport tail)
4. `UserEvent::AutoApprove` injection path (main-thread authoritative resolution + pre-injection reconfirmation + fixed byte sequence per agent × signature)
5. Consumed flag (cell-hash lock + cap on consecutive approvals)
6. Input-conflict suppression (IME/paste/recent user input 3s)
7. Runaway breaker (auto-OFF + attention after 6 in a 60s window)
8. Visualization (tab/sidebar badge + fire flash)
9. Audit log (16-entry ring buffer + sidebar card display)
10. Fires even on the focused tab (collision avoidance delegated to item 6)

### Out-of-scope

- Automating Bash command execution approval (v2: only leave design room for denylist extension)
- Config-file regex table / user-defined patterns
- CLI flag injection (e.g. `--dangerously-skip-permissions`) approach
- Structured OSC channel (parked)
- Improving detection for agents that fall into `Generic` due to node wrappers, etc.
- Full-text dangerous-word parser
- Dedicated audit-log modal (v2)
- Prioritizing don't-ask-again responses

## Open Questions / Deferred Decisions

- **Structured OSC channel** (CLI declares approval requests via control sequences, the VS Code approach): parked. v1 uses on-screen detection. Since the detection core is isolated in the pure function `detect`, a future swap is localized
- **CLIs behind a node wrapper** falling into `Generic` is a known limitation (`sidebar.rs:731`): out of scope for v1. Improving detection is a separate task
- **Extending the Bash approval denylist** (a substring denylist of roughly 20 lines): v2 candidate. The signature matrix keeps a structure that can be extended (NFR-5)
- **Dedicated audit-log modal**: v2 candidate (v1 keeps it inside the sidebar card only)
- **If Codex/agy signature capture (T-1) is delayed**: proceed with Claude Code alone first; agents not yet captured simply remain no-fire (the fail-safe design allows a safe partial release)
- Concrete animation for the fire flash: follow the existing anim foundation (UI_ACCENT/token) conventions at implementation time
