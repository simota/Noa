# Spec: Sidebar per-window session display

- **slug:** sidebar-per-window-sessions
- **status:** locked (2026-07-05)
- **owner:** simota
- **build-path decision:** apex (single unattended run) — fallback: feature / manual implementation
- **quality gate:** PASS (Judge findings S1–S5 all addressed)

## L0 — Vision

- **Problem:** With multiple windows in use, each window's sidebar shows a mix of sessions from every window, making it impossible to tell "which sessions belong to this window."
- **Target:** users who run noa with multiple windows (= the developer themself).
- **Job-to-be-done:** looking at the sidebar, only sessions belonging to the window currently being operated are listed, and that window's state (busy / attention) is visible at a glance.
- **Success definition:** each window's sidebar displays, counts, and hit-tests only its own window's sessions, with no regression to existing GC / delta / scroll behavior.

## Reuse / constraint findings (Lens scan, 2026-07-05)

- The data model is already keyed per-window: `SessionCardId { window_id: SessionWindowId, pane_id }` (`crates/noa-app/src/session_store.rs:55-65`).
- The cause of the cross-window display is purely on the rendering side: `sidebar_draw_model` uses `session_store.ordered_ids()` (all cards) with no filtering (`crates/noa-app/src/app/sidebar.rs:772`).
- Other call sites sharing the same `ordered_ids()` that need to change: hit_test (`sidebar.rs:532`), card_rect reverse lookup (`sidebar.rs:693`), draw model (`sidebar.rs:765,772`). These three share an invariant that they must use the same list.
- Header counts are also aggregated globally: `attention_count()` (`sidebar.rs:797`), `busy_count()` (`sidebar.rs:813`).
- Confirmed as not needing changes: GC (`reconcile_session_store`, correctly cross-window), scroll (`WindowState.sidebar_scroll` already per-window), delta path (`io_thread.rs:457` already embeds window_id), `clear_session_bell_for_window` (already filtered).
- Constraint (NFR): `session_store.rs` must stay GUI-independent (no winit/wgpu imports; there's a self-scanning test for this). The filter argument must be `SessionWindowId`-based.
- `SessionStore` is owned solely by the main thread, lock-free. Adding a filter method is less invasive than splitting the store per window.
- Related: Overview (`app/overview.rs`) references the same store across all windows. Existing spec: `docs/specs/session-sidebar.md`.

## FRAME decisions (user confirmed)

1. Problem Statement: settled above in L0.
2. **Overview keeps showing all windows** (sidebar = own window, Overview = whole-picture overview — a deliberate division of roles; the resulting mismatch in visible sets is intentional).
3. **Attention is fully per-window** (only shows/counts attention for the current window; attention in other windows is noticed there instead).
4. **The sidebar_visible toggle stays app-wide as-is**. Making it per-window is recorded as an Open Question.

## EXPAND — candidates and selection

- **A: add filter methods to SessionStore** (`ordered_ids_for_window` / `busy_count_for_window` / `attention_count_for_window`) + swap in the 5 call sites in sidebar.rs — **adopted (user pick)**. Minimally invasive, consolidates the filter logic in one place, unit-testable.
- B: split the store per window (`HashMap<SessionWindowId, SessionStore>`) — **rejected**: changes would ripple across GC, delta, and Overview paths, and conflicts with keeping Overview showing everything.
- C: filter at the view layer (retain in App) — **rejected**: duplicates the filter in 3 places, breaks the invariant that hit_test/draw share the same list, and isn't testable.

## CHALLENGE — stress-test results and confirmed direction A′ (user confirmed)

Three corrections to the original proposal A (from the Void/Ripple/Omen panels, based on code inspection):

1. **The filter granularity is `WindowGroupId` (logical window)**. A macOS native tab is a separate winit `WindowId` even within the same logical window (`app.rs:125-151`). Filtering at winit WindowId granularity would degenerate to a single-tab list, hiding sibling tabs. The App side computes the `HashSet<SessionWindowId>` of winit WindowIds belonging to the focused window's group and passes it to the store (precedent: `group_running_program_count` `app.rs:1534-1541`).
2. **There are 6 call sites**, not the original 5. In addition, `handle_sidebar_wheel`'s `session_store.len()` (`sidebar.rs:719`) — if left alone, the scrollable range would drift from the actual card count.
3. **Only one filter method is added**: `ordered_ids_for_windows(&HashSet<SessionWindowId>) -> Vec<SessionCardId>`. busy/attention counts are aggregated inline within `sidebar_draw_model` from the already-filtered `ids` (two dedicated count methods would be YAGNI). The wheel handler's count uses `.len()` on the filter result.

Additional findings:
- Overview only uses `get(&card_id)` and never calls `ordered_ids` (`overview.rs:484,1272`) → "keep Overview global" is therefore satisfied automatically with zero changes.
- `selected_id` is always within its own group (`sidebar.rs:781`) → no dangling references.
- Attention blink/float is driven by per-card state → no dependency on the global set. Not showing attention from other windows is the intentional loss laid out in the scope decision.
- Existing tests remain unbroken (the `ordered_ids`/`busy_count`/`attention_count` bodies themselves are kept in place).

Complete list of call sites (all must consistently use the same filtered set — an invariant):
| # | Site | Location |
|---|------|------|
| 1 | hit_test | `sidebar.rs:532` |
| 2 | card_menu_anchor reverse lookup | `sidebar.rs:693` |
| 3 | scroll content_height count | `sidebar.rs:719` |
| 4 | draw model | `sidebar.rs:765` (→ flows into layout at 766) |
| 5 | attention count (aggregated in draw) | `sidebar.rs:790-797` |
| 6 | busy count (aggregated in draw) | `sidebar.rs:806-813` |

## SHAPE — proposal (user confirmed)

- **Problem:** with multiple windows in use, the sidebar mixes sessions from all windows.
- **Solution:** restrict the sidebar's list/hit-test/scroll/header counts to the sessions of its own logical window (`WindowGroupId`, including native tabs). Add one method, `SessionStore::ordered_ids_for_windows(&HashSet<SessionWindowId>)`, plus an App-side group→SessionWindowId set helper, and swap in 6 call sites in `sidebar.rs`.
- **In-scope:** group-restricting the card list / hit_test / scroll region / busy & attention counts, the App-side group→window set helper (derivation logic as a pure function), unit tests.
- **Out-of-scope:** Overview (stays global, zero changes), making `sidebar_visible` per-window, other-window attention indicators, structural changes to GC/delta/scroll state.
- **Assumptions:** "window" means `WindowGroupId` (AppKit logical window). An empty filter result is acceptable, rendering the header only (equivalent to the existing empty-store behavior). If the target window_id is missing from the `windows` map or its group can't be resolved, treat it as an empty set (degrading to header-only rendering).

## L1 — Requirements

| ID | Type | Requirement |
|----|------|-----|
| R1 | Functional | The sidebar shows only session cards belonging to the `WindowGroupId` of the window being drawn. |
| R2 | Functional | Sessions of native tabs (separate winit `WindowId`) within the same logical window are also shown in a sibling tab's sidebar. |
| R3 | Functional | hit_test, card-menu-anchor reverse lookup, and the draw model all use the same filtered list (maintaining the list invariant). |
| R4 | Functional | The sidebar's scrollable range (content_height) is computed from the post-filter card count. |
| R5 | Functional | The header's busy / attention counts are aggregated only over sessions in the own group. |
| R6 | Functional | Card ordering after filtering still follows the existing sort (attention float → window_id → pane_id). |
| R7 | Non-functional | `session_store.rs` stays GUI-independent (no winit/wgpu imports; filter arguments are `SessionWindowId`-based). |
| R8 | Non-functional | No change to GC (reconcile) / delta path / per-window scroll state / Overview behavior. Existing tests pass unmodified. |

## L2 — Detail

- **SessionStore API** (`crates/noa-app/src/session_store.rs`):
  ```rust
  /// Returns only the cards whose window_id is in `windows`, in the same sort order as ordered_ids.
  pub fn ordered_ids_for_windows(
      &self,
      windows: &HashSet<SessionWindowId>,
  ) -> Vec<SessionCardId>

  /// (busy, attention) counts among cards belonging to `windows`.
  pub fn counts_for_windows(
      &self,
      windows: &HashSet<SessionWindowId>,
  ) -> (usize, usize)
  ```
  The existing `ordered_ids` / `busy_count` / `attention_count` are kept in place (to preserve existing tests). `HashSet` is from `std::collections` (already used in session_store.rs, no GUI dependency).
- **Derivation of a group→window set (pure function):** given a list of `(SessionWindowId, WindowGroupId)` pairs and a target group, return the `HashSet<SessionWindowId>` for that group, factored out as a pure function (winit-independent, unit-testable). A thin App-side wrapper builds the pair list from the `windows` map + `window_order` and calls it. Precedent: `group_running_program_count` (`app.rs:1534-1541`).
- **Call-site swaps (6 sites, all sourced from the same set):**
  1. hit_test (`sidebar.rs:532`)
  2. card_menu_anchor reverse lookup (`sidebar.rs:693`)
  3. count in `handle_sidebar_wheel` (`sidebar.rs:719`, `session_store.len()` → filtered `.len()`)
  4. `sidebar_draw_model` (`sidebar.rs:765`, → flows into layout at 766)
  5. attention count → `counts_for_windows` (`:790-797`)
  6. busy count → same (`:806-813`)
- **Empty filter result**: renders the header only (same path as the existing empty-store behavior).

## L3 — Acceptance Criteria

| ID | Requirement verified | Criterion | Verification method |
|----|--------------|------|----------|
| AC-1 | R1 | With cards in two groups A/B, `ordered_ids_for_windows({A's window set})` contains none of B's cards. | unit test |
| AC-2 | R2 | Cards for multiple `SessionWindowId`s within the same group (equivalent to native tabs) are all included in the result. | unit test |
| AC-3 | R6 | The order of the filtered result matches the relative order of `ordered_ids` (attention float → window_id → pane_id). | unit test |
| AC-4 | R3 | A grep for `self.session_store.ordered_ids()` and `self.session_store.len()` within `sidebar.rs` returns 0 hits, and all 6 sites are sourced from the filtered set (local `store.len()` etc. within tests are excluded). | grep + code review |
| AC-5 | R4 | The wheel handler's content_height computation uses the post-filter count, and scroll clamping matches the number of displayed cards. | code review + manual check |
| AC-6 | R5 | When busy/attention sessions exist only in another group, `counts_for_windows` returns (0, 0) and the own window's header count shows 0. | unit test + manual check |
| AC-7 | R7 | `session_store.rs`'s GUI-independence self-scanning test passes. | `cargo test -p noa-app` |
| AC-8 | R8 | `cargo test --workspace` passes entirely, and the existing `session_store.rs` tests pass unmodified. `cargo clippy --workspace` is clean. | CI-equivalent command |
| AC-9 | R1/R2 | Real GUI: with 2 logical windows, one of them having a cmd+t tab added, visually confirm each sidebar shows only its own group's sessions, including the sibling tab's sessions. | manual GUI check |
| AC-10 | R2 | The pure function deriving the group→window set correctly returns exactly the `SessionWindowId`s of the target group from a mixed pair list (multiple groups, multiple tabs), with no extras or omissions. | unit test |

## Open Questions / Deferred Decisions

- Making sidebar_visible per-window (out of scope this time, future consideration).
