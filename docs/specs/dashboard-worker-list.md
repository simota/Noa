# Agent dashboard: one worker list, attention first

- slug: dashboard-worker-list
- status: design (2026-11-26)
- owner: simota
- scope: `widgets/agent-dashboard/` (a standalone crate with its own
  `[workspace]`) and `docs/FEATURES.md`. No change to Noa's workspace crates,
  `vendor/`, `scripts/`, CI, any `Cargo.toml` dependency list or
  `Cargo.lock`.
- builds on: `docs/specs/agent-dashboard-improve.md` (key router, pager,
  health, drawer) and `docs/specs/agent-dashboard-mock.md`. Their
  conventions hold: headless tests through `Dashboard::dispatch` and
  `prepare`, `TextStore::new(Default::default())`, Japanese UI text.
- out of scope: Board Timeline outcome integration, progress % / ETA, new
  short task titles, new dependencies.

## Goal

Replace the five-panel operations view with one list, one row per **worker
pane**, attention first. A pane can receive more tasks: a row counts panes,
not tasks or completed jobs (the in-app help and `docs/FEATURES.md` say so).

| AC | What |
| --- | --- |
| AC1 | Default view = one list, one row per worker pane; the five panels are not built |
| AC2 | 現在 / 要対応 / 終了 switch; 要対応 ⊂ 現在; membership tests (quiet, notify, failed-with-pane, closed-failed, closed-ended) |
| AC3 | One attention predicate used by the count, the 要対応 view, `state:waiting`/`state:要対応`, analytics `waiting`, per-parent counts, top ordering; count = rows under the same filter |
| AC4 | Row = state · task + report + parent · kind + label · wait/update · one action; no sparkline, gauge, model, branch, report count, longest tag; model/effort/branch/cwd in a collapsed 実行情報; branch/cwd searchable |
| AC5 | approval > input > failed > running; longest wait first within a kind; running stable (parent, start); a clock advance does not reorder; selection kept by identity |
| AC6 | quiet = 実行中 + 「出力未変化 N 分」; 終了 never 成功, with 閉じた / 結果あり; missing task = 「作業内容未取得」, never the label |
| AC7 | 「絞り込み外に要対応N件」 with a clear-filter action, only when such items exist |
| AC8 | One primary action per state, wired to open-pane / drawer / result |
| AC9 | Drawer order: reason or report, actions, results/diff, instructions + log (collapsed), run info (collapsed) |
| AC10 | J/K/arrows, Enter, Space, `/` unchanged; keys 1–5 and saved prefs migrated (old file loads, test); 分析 reaches stats/map/timeline; help + FEATURES updated |
| AC11 | 同期済み when fine; 「最後の受信から N 秒」 when stale or disconnected |
| AC12 | Factory check no worse than the baseline below |

Needs a person at a window (not closable here): rows visible at 1440×900 and
1100×700, the 3-second discovery, visual emphasis.

## Findings (verified 2026-11-26 on `orch/design` at `1a6cc29`)

### The model (`src/fleet.rs`, `src/live.rs`, `src/live/journal.rs`)

- `State` has five values: `Running, Permission, Input, Finished, Error`
  (`fleet.rs:5-12`). `quiet` is a separate bool (`fleet.rs:138`), set by the
  feed when a running pane's preview has not changed for `QUIET_SECS` = 120 s
  (`live.rs:39`, `live.rs:1006-1008`). Quiet never applies outside `Running`.
- `Worker::need()` (`fleet.rs:165-179`): `None` for a closed worker
  (`closed_pane.is_some()`, `fleet.rs:197-199`), else Permission → Approval,
  Error → Failure, Input → Question, Running+quiet → Quiet, Running/Finished
  with `attention` → Notice, else `None`. `Need::tier()` puts Approval and
  Question in `Decision`, Failure in `Check`, Notice and Quiet in `Info`
  (`fleet.rs:89-95`).
- So the goal's attention set (approval, input, failed; not notify, not quiet)
  is exactly `need().tier() != Tier::Info`, which is also
  `!is_closed() && state ∈ {Permission, Input, Error}`.
- A closed worker is rebuilt from its spawner's journal; its state is `Error`
  when the last wait outcome is `"error"`, else `Finished`
  (`journal.rs:566-572`). Its `since` is "N 分前に閉じた" or "閉じた"
  (`journal.rs:542-545`); `has_result` and `result` come from the journaled
  result file (`journal.rs:586-587`).
- `task` is the first non-empty line of the last task sent
  (`journal.rs:61-63`, `first_line` `journal.rs:138-143`, set at
  `journal.rs:210`); live workers get it from their spawner's journal
  (`live.rs:1201-1205`) and it is empty until a task is sent. **The full
  instruction text is not kept anywhere in `Worker`**: the drawer can show
  only this first line.
- `detail` is the agent's latest report, prefixed by `exit N` when the pane
  exited (`live.rs:1499-1514`); for a closed worker it is
  "最後の待ち結果: {reason}" (`journal.rs:575-579`).
- `wait_minutes`: time in state for Permission / Input / Error, time since the
  last preview change for a quiet worker, 0 otherwise (`live.rs:1538-1542`).
  `since` is the text of that time: "N 前に出力" / "出力なし N" (quiet) /
  "N 前に完了" / the time in an attention state, "以上" when the feed did not
  see the state begin (`live.rs:1489-1497`). There is no numeric "time since
  last output" for a non-quiet running worker; `since` carries it as text, and
  is empty when the feed never saw a change.
- `Worker::structure()` zeroes only `since`, `wait_minutes`, `reports`,
  `started_minutes`, `took_minutes`, `lines_per_tick`, `freshness`, `phases`
  (`fleet.rs:251-263`). State, quiet, closed, task, detail, result stay
  structure: any change to them rebuilds the screen (`mod.rs:723-749`).
- The live fleet lists open workers in pane-id order (`live.rs:1215`), then
  closed ones newest-ended first (`journal.rs:519-521`, appended at
  `live.rs:1235-1239`).

### The dashboard today (`src/dashboard/`)

- `View` (`mod.rs:102-127`) is `Copy` and carries `shown: [bool; 5]`,
  `first: [usize; 5]` (pager), `expanded` / `info_open` (queue), `selected`,
  `detail`, `result`, `tab`, `grouping`.
- `fill_body` (`mod.rs:1300-1414`) builds, for `Tab::Operations`, a NOW row
  (queue 400 px, running grows, done 270 px) and a HISTORY row 256 px high
  (timeline, graph 430 px) from `view.shown` (`mod.rs:1318-1382`;
  `panel.rs:14-18`, `panel.rs:95-108`). The other tabs build `analytics`,
  `mindmap` and `gantt` (`mod.rs:1383-1405`).
- Running cards carry kind/model/branch tags, the quiet tag, task (2 lines),
  detail, an output sparkline, a freshness gauge and "開始 · 報告 N 回 ·
  since" (`cards.rs:253-315`, `cards.rs:37-44`). Queue cards add the need,
  urgency steps and the queue's 人待ち合計 tag (`queue.rs:89-179`,
  `urgency.rs`).
- The four "要対応" meanings the goal names:
  - queue count = `waiting.len() - info` → excludes notice/quiet
    (`queue.rs:111-112`);
  - queue's out-of-filter caption counts `Tier::Decision` only, so failures
    outside the filter are not announced (`queue.rs:127-137`,
    `queue.rs:80-82`);
  - search `state:waiting` / `state:要対応` = `need().is_some()`
    (`filter.rs:83`) → includes notice/quiet;
  - analytics `waiting` = `need().is_some()` (`analytics.rs:45`);
  - per-parent: the map's orchestrator hub counts `tier() != Info`
    (`mindmap.rs:141-144`) but the branch hub counts `need().is_some()`
    (`mindmap.rs:189`);
  - header urgency counts include 静止 (`header.rs:42-49`, `header.rs:270`).
- Selection order (`select.rs:166-202`) is queue cards, then running, then
  done, of shown panels; `select::reselect` keeps a pane by id
  (`select.rs:234-241`). After a live update a selected pane that left the
  order is dropped with a notice (`mod.rs:871-906`, `mod.rs:739-743`).
- A time-only snapshot updates registered `Pulse`s in place and calls
  `reorder_queue`, which **moves queue cards** by wait (`mod.rs:944-1010`,
  `mod.rs:1040-1101`): today a clock tick can reorder the queue.
- The pager: each list is a clipped column of whole items
  (`panel.rs:168-182`, `pager.rs:1-5`). On a freshly built list (no
  `PageMemory` seen yet) `fit` sets `view.first = 0` and aims at the
  selection (`pager.rs:426-433`): **every structural rebuild today scrolls the
  lists back to the top** and pages the selection into view.
- Header (`header.rs:126-255`): title, health dot + status, urgency counts
  (clickable, `mod.rs:2101-2120`, `mod.rs:2124-2141`), 最長 tag
  (`header.rs:192-201`, `header.rs:462-472`), one chip per hidden panel
  (`header.rs:203-207`), filter field, match count, `?`, グループ control.
- Footer (`mod.rs:378-416`): notice text + tab control 運用 / 統計 / マップ /
  タイムライン (`mod.rs:79-100`).
- Keys (`keymap.rs:101-241`): `1`–`5` toggle panels (`keymap.rs:187-221`,
  help row `keymap.rs:97-99`), `T` cycles tabs (`keymap.rs:166-172`), plus
  J/K/↑/↓, N, Enter, Space, S, X, ⇧X, D, G, `/`, Esc, `?`. The help is built
  from `BINDINGS` (`help.rs:26-41`).
- Prefs (`prefs.rs:24-32`): `tab, grouping, shown, map_axis, time_scale,
  filter`, written as `"version": 1` with `"shown": {queue, running, done,
  timeline, graph}` (`prefs.rs:111-129`). `from_json` takes each known value
  and defaults the rest; it never reads `version` (`prefs.rs:135-165`). A
  loaded view is taken as saved and the file is written only when the view
  changes (`prefs.rs:184-206`).
- Health (`health.rs:170-194`): Live → "ワーカー N · 同期 X前"; Stale (no
  answer for `STALE_AFTER` = 10 s, `health.rs:20`) → "古いデータ（同期 X前）·
  ワーカー N"; Down with data → "切断: 古いデータ（X前）· reason"; Down
  without → "切断: reason". A lost link keeps the last fleet
  (`health.rs:277-283`). The header ticks every frame (`mod.rs:931-942`).
- Drawer (`drawer.rs:557-915`) order today: head (mark, label, kind) →
  model/branch/閉じた/結果 tags → 状態 → ペイン (ID, 親, cwd) → 依頼 (task,
  "不明" when empty, `drawer.rs:678-684`) → 画面の末尾 → 結果 → 時間 →
  指示・返信 (send; refused for closed and Permission, `drawer.rs:791-799`) →
  まとめて閉じる row → fixed button row 開く / ペインを閉じる / 差分を見る /
  閉じる (`drawer.rs:851-901`). Its time labels are refreshed **by child
  index** within the 時間 column (`drawer.rs:987-1022`).
- Primary paths that exist: open pane = `Dashboard::open` → `Command::Focus`,
  refused for a closed worker (`mod.rs:1491-1513`); drawer =
  `set_detail` (`mod.rs:1653-1660`); send = `Action::StartSend`
  (`mod.rs:2215-2240`), refused for Permission
  (`drawer.rs:46-47`, `mod.rs:2222-2225`, `mod.rs:1828-1830`); result =
  the drawer's 表示 sets `view.result` and re-reads (`mod.rs:2647-2651`).
- noa-gpui-core 0.2.1 has `DisclosureGroup` (titled section that hides its
  children when collapsed, `.expanded(bool)`, emits `Toggled`;
  `~/.cargo/registry/src/*/noa-gpui-core-0.2.1/src/widgets/panes.rs:1372-1440`,
  exported in `widgets/mod.rs:128`). Nothing in `widgets/` uses it yet.
- Search: plain terms match label, kind, model, branch, task
  (`filter.rs:39-45`); `branch:` exists (`filter.rs:103`). **`cwd` is not
  searched today** (the goal's "branch/cwd stay searchable" is only half
  true).
- Dead-code coupling: `graph::mark` is used by analytics, mindmap, gantt and
  the drawer (`analytics.rs:18`, `mindmap.rs:20`, `gantt.rs:16`,
  `drawer.rs:24`); `timeline::{LANE_SHARES, span_text, timeline_span}` and
  `panel::group_header` by gantt (`gantt.rs:17-19`); `cards::CLOSED_TAG` by
  the drawer (`drawer.rs:637`); `cards::shows_done` and `queue::cards` by
  select (`select.rs:7-10`). With `-D warnings`, whatever only the panels
  used must be deleted in the unit that stops building them.

### Baseline checks (Linux, this commit, `CARGO_NET_OFFLINE=true`)

| Check | Result |
| --- | --- |
| `cargo fmt --check --manifest-path widgets/agent-dashboard/Cargo.toml` | pass |
| `cargo clippy --all-targets --manifest-path … -- -D warnings` | pass |
| `cargo test --manifest-path …` | 360 passed, **1 failed**, 2 ignored |

The failure is `dashboard::tests::header_fit::the_drop_order_is_kept`
(`header_fit.rs:268`, "1000 px narrowing: [761.0, 11.7, 226.56, 24.6] leaves
[16, 984]"): header widths measured with Linux fonts. The 2 ignored are
`live/tests/performance.rs:80,110` (manual measurements). "No worse than the
baseline" (AC12) means: fmt and clippy pass, and no test that passes at the
baseline fails, except tests a unit deletes or replaces on purpose, each named
in the spec's implementation record. `scripts/verify.sh` passes on this
commit and reports "not covered: the dashboard's tests (macOS only; it is
built)": on Linux it builds the dashboard but does not run its tests, so the
three cargo commands above are the dashboard's gate here.

## Definitions (land in U1, `src/fleet.rs`)

```rust
/// What a row says the worker is doing (the 状態 column).
pub enum Status { Approval, Input, Failed, Running, Ended }

/// Which list a worker is in.
pub enum Scope { Current, Attention, Ended }

impl Worker {
    /// The one definition of 要対応: its pane is open and it waits on an
    /// approval, an input, or failed. Not a notice, not quiet.
    pub fn needs_attention(&self) -> bool;   // !is_closed() && state ∈ {Permission, Input, Error}
    pub fn status(&self) -> Status;
    pub fn in_scope(&self, scope: Scope) -> bool;
}
```

| Worker | `status()` | 現在 | 要対応 | 終了 | qualifiers |
| --- | --- | --- | --- | --- | --- |
| open, Permission | Approval 承認待ち | ✓ | ✓ | | |
| open, Input | Input 入力待ち | ✓ | ✓ | | |
| open, Error | Failed 失敗 | ✓ | ✓ | | 結果あり |
| open, Running | Running 実行中 | ✓ | | | 出力未変化 N 分 when quiet |
| open, Running / Finished, `attention` (notice) | Running / Ended | as its state | | as its state | none (see Q4) |
| open, Finished | Ended 終了 | | | ✓ | 結果あり |
| closed, outcome error | Failed 失敗 | | | ✓ | 閉じた, 結果あり |
| closed, other | Ended 終了 | | | ✓ | 閉じた, 結果あり |

- `Scope::Current` = `!is_closed() && state != Finished`; `Scope::Ended` = the
  rest; `Scope::Attention` = `needs_attention()`. So 現在 and 終了 partition
  every worker, and 要対応 ⊂ 現在 (a closed worker never needs attention).
- `Status::title()`: 承認待ち / 入力待ち / 失敗 / 実行中 / 終了. No word for
  success exists in the row vocabulary.
- `Status::rank()`: Approval 0, Input 1, Failed 2, Running 3, Ended 4.
- `Scope::{title, name, parse}`: 現在 / 要対応 / 終了; `current`,
  `attention`, `ended` (prefs).
- `Fleet::parent_name(&Worker) -> String`: the text the drawer computes today
  (`drawer.rs:653-663`: parent worker's label, else "ペイン N", なし for an
  orchestrator or `ALONE`, 不明, else the orchestrator name), moved here so
  the row and the drawer say the same.

## Approach per AC

### AC1: one list (U3)

- New `src/dashboard/list.rs`: `rows(fleet, scope, filter) -> Vec<&Worker>`
  (membership + order, pure) and `build(tree, body, …, registry)` (toolbar +
  rows). `fill_body` for `Tab::Operations` calls only `list::build`; the
  NOW/HISTORY rows and `Panel` go (`mod.rs:1317-1382`).
- Delete what only the panels used: `cards.rs`, `queue.rs`, `urgency.rs`, the
  panel builders `graph::graph` and `timeline::timeline` / `fill_lanes`, the
  `Panel` enum and panel layout in `panel.rs` (keep `card_list`,
  `group_header`, `indicator`, `nothing_matches`, `mark_card`, `empty`
  rewritten without panel keys). `CLOSED_TAG` moves to `list.rs` (the drawer
  imports it from there).
- `View`: drop `shown`, `expanded`, `info_open`; `first: [usize; 5]` →
  `first: usize`; add `scope: Scope`. `Registry::lists` → `list:
  Option<ListParts>`; `pager::fit` / `pager::wheel` take one list. The
  test-only `Dashboard::panels` becomes `list: Option<WidgetId>`.
- `select::order` = `list::rows(...)` panes; `next_waiting` (N) walks the
  `needs_attention()` rows in list order. `Dashboard::waits` and
  `queue_buttons` go; `watch` asks only for `view.detail`'s tail.
- Test (`tests/list.rs`): a laid-out default dashboard has exactly one list,
  `registry.list` is `Some`, and no label in the tree reads 作業中 / 完了 /
  状態の推移 / 関連 as a panel title.
- Risks: 205 of the 246 tests in `src/dashboard/tests/` sit in files that
  reference `Panel` or `shown` (`grep -c`; `paging.rs` alone has 130
  `Panel::` uses); most are rewritten or deleted, so coverage of paging,
  selection and the empty state must be rebuilt on the list, not dropped.
  The time-only path must stop calling `reorder_queue`.

### AC2: 現在 / 要対応 / 終了 (U1 membership, U3 switch)

- U1: `Scope`, `Worker::in_scope` (table above) with unit tests in
  `fleet.rs`: running, quiet (現在 only), notice on a running and on a
  finished worker (never 要対応), open failed (現在 + 要対応), closed failed
  and closed finished (終了 only), open finished (終了), and the invariants
  "要対応 ⊂ 現在" and "現在 ⊎ 終了 = all" over `canned_with_closed()`.
- U3: the list toolbar starts with a `SegmentedControl` 「現在 N」「要対応 N」
  「終了 N」, N counted over the filter's target set (AC3). The three are
  shown side by side, never summed. Selecting one sets `view.scope` and
  rebuilds (`change_view`). Keys `1`/`2`/`3` do the same (AC10).
- Empty scope: 現在 「作業中のワーカーはいません」, 要対応 「要対応はありません」,
  終了 「終了したワーカーはありません」; with a filter on, 「該当なし」. No
  worker at all keeps today's `panel::empty` messages, without the panel
  keycaps (`panel.rs:204-233`).
- Risk: membership depends only on structure fields, so counts change only
  on a rebuild or a filter refill; that is correct, and a test pins it.

### AC3: one attention predicate (U1, U3)

- U1 switches every consumer to `Worker::needs_attention()`:
  `filter.rs:83` (`state:waiting` / `state:要対応`), `analytics.rs:45`,
  `mindmap.rs:141-144` and `mindmap.rs:189` (per-parent / per-branch hub
  counts). `need:` qualifiers keep their per-need meaning (`need:any` is
  still "any need", notices and quiet included; documented in the help's
  `/` row by U5).
- U3: the 要対応 count in the switch, the 要対応 view, the top ordering and
  the out-of-filter notice all read `needs_attention()`; the header urgency
  counts (`header.rs:42-49`, `259-282`), the 最長 tag and their click filter
  (`mod.rs:2101-2141`) are removed (their job moves to the switch).
- "Search + parent filter": the parent filter is the search's `parent:` /
  `p:` qualifier (`filter.rs:104`); there is no second filter. Target set =
  `fleet.workers.filter(filter.matches)`.
- Tests: U1 (`tests/attention.rs`): over a fleet with one of each kind,
  `matches("state:waiting")` and `matches("state:要対応")` equal the
  `needs_attention()` set; `compute_metrics(..).waiting` equals its size;
  the orchestrator and branch hub counts equal it per group. U3
  (`tests/list.rs`): for queries `""`, `"parent:release"`, `"agent:codex"`,
  `"-state:input"`, the 要対応 segment's N equals the number of rows the
  要対応 view lays out, and its first rows are exactly the attention rows of
  現在 in the same order.
- Risk: `tests/actions.rs:34` asserts `state:waiting` = {13,14,15,16,19}; 13
  is quiet and leaves (U1 updates it to {14,15,16,19}).

### AC4: the row (U3; drawer part U4)

- One row = a fixed-height container (`ROW_HEIGHT` = 60 px, padding 8 / 12,
  `flex_shrink: 0`), a focus group named by the worker's label, the
  selection bar of `panel::mark_card`, five columns:

  | # | Column | Width | Content |
  | --- | --- | --- | --- |
  | 1 | 状態 | 112 px | `status().title()` 14 px (weight 700 and `need_color` for Approval/Input/Failed; muted normal weight for Running/Ended); below, 12 px muted qualifiers joined by " · " (閉じた, 結果あり, 出力未変化 N 分) |
  | 2 | 作業・最新の報告 | grows, min 0 | task 14 px, one line, `Truncation::Tail`, or 「作業内容未取得」 muted; below, 12 px muted one line: `detail` · 親 `parent_name` (each part left out when empty) |
  | 3 | エージェント | 168 px | kind tag (`Tone::Accent`, small) + label 12 px, one line, cut |
  | 4 | 待ち／更新 | 104 px | attention: 「待ち {since}」 in the need colour; otherwise `since` muted ("N 前に出力", "出力なし N", "N 前に完了", "N 分前に閉じた"); empty → 「—」 |
  | 5 | 操作 | 104 px | one small button (AC8) |

  Fonts: 14 px primary, 12 px secondary, so every row has the same height
  whatever its text.
- Not in a row: sparkline, freshness gauge, model/effort, branch, cwd, report
  count, start time, cumulative wait, longest-wait tag, screen tail.
- In place updates: `Pulse::RowTime(pane)` (column 4) and
  `Pulse::RowQuiet(pane)` (the qualifier line) replace `Waited`, `Longest`,
  `QueueSince`, `QueueCard`, `QuietTag`, `Output`, `Freshness`,
  `RunningCaption`, `DoneCaption` (`mod.rs:291-304`).
- Search keeps model and branch in plain terms (`filter.rs:39-45`). U1 adds
  a `cwd:` / `c:` qualifier (not a plain term: every cwd shares the repo
  path, so a plain term like `noa` would match all; see Q3).
- Clicking a row (not its button) selects it: rows register
  `(row, pane)` and `try_direct_click` (`mod.rs:2124-2168`) checks them as it
  does lanes and nodes.
- Tests (`tests/list.rs`): a row's label texts are exactly
  [status (+ qualifiers), task, report·parent, kind, label, time, action];
  no `Chart` or `LevelIndicator` under the list; the strings of `model`,
  `effort`, `branch`, `cwd` and "報告" appear under no row; every row's
  laid-out height equals `ROW_HEIGHT` (one long task, one empty task). U1:
  `branch:` and `cwd:` match.
- Risk: label widths differ per platform; assert heights and the presence
  of texts, never pixel widths.

### AC5: order and stability (U3)

- `list::rows` order for 現在 / 要対応: attention rows by `status().rank()`,
  then `wait_minutes` descending (`total_cmp`), then pane ascending; then
  running rows by parent group (first appearance of
  `(orchestrator, orchestrator_pane)` in `fleet.orchestrators`), then
  `started_minutes` descending (`None` last), then pane ascending. 終了:
  open ended rows in fleet order, then closed rows in fleet order (newest
  ended first, `journal.rs:519-521`); see Q5.
- The order is computed **only when the list is built** (a structural
  snapshot or a view/filter change). `refresh` updates the row pulses in
  place and moves nothing (`reorder_queue` and `expand_in_place` are
  deleted). Within one attention kind all waits grow at the same rate and
  `started_minutes` likewise, so a rebuild after time passed gives the same
  order a rebuild before gave.
- Selection by identity: `select::reselect` already keeps a pane by id
  (`select.rs:234-241`); `view.reveal` is set only by the user's own moves.
- Scroll position: add `View::anchor: Option<u64>`, the first row on the
  page, written by `pager::fit`. On a freshly built list `fit` starts from
  `item_of(anchor)` (else `first` clamped) and `Aim::Stay`, instead of
  `first = 0` and aiming at the selection (`pager.rs:430-433`). So a new
  attention row above does not push the rows the user is reading, and the
  selection never moves.
- New attention while operating: when a structural snapshot adds a pane to
  the attention set, the notice line says 「要対応が増えました: {label}（{status}）」
  (more than one: 「要対応が N 件増えました」); selection and page stay.
- A selected worker that leaves the scope shown (it finished while 現在 is
  shown) is deselected with today's notice (`mod.rs:226-231`), extended with
  where it went: 「… は 終了 に移りました」. An open drawer stays on it while
  it still matches the filter (`mod.rs:893-904`).
- Tests (`tests/list.rs`): order over a crafted fleet (two approvals with
  waits 2 and 9, input, failed, three running under two parents) is
  [approval 9, approval 2, input, failed, running by parent then start];
  applying a time-only snapshot (same structure, waits swapped and
  `started_minutes` changed) leaves the row order and the page's first row
  unchanged; select a running worker, apply a structural snapshot where it
  turns Permission: it is still selected (by pane), `view.first`'s row is
  the same pane as before, and the notice names it; a new approval arriving
  while another row is selected leaves the selection and says so.
- Risk: `pager.rs` is shared with no other list after U3, but its tests in
  `paging.rs` encode "rebuild returns to top"; they are rewritten.

### AC6: state wording (U1 status, U3 render)

- U1 `Status` + `Scope` give the words; `Status::Ended` is 終了 for closed
  and open finished alike; nothing says 成功 / 完了 in a row.
- U3: quiet → 状態 実行中 + qualifier 「出力未変化 {minutes_text(wait_minutes)}」
  (e.g. "出力未変化 3 分"; the house format of `minutes_text`,
  `fleet.rs:333-342`); 閉じた when `is_closed()`; 結果あり when `has_result`.
  Empty `task` → 「作業内容未取得」, never `label` (the label is in column 3).
- Tests: U1 `status()` per row of the table; U3 render: canned pane 13 shows
  実行中 and "出力未変化 3 分", no 失敗 / 停止; a closed finished worker shows
  終了 · 閉じた · 結果あり and no 成功 / 完了; a worker with an empty task
  and label "x" shows 「作業内容未取得」 and "x" only in column 3.

### AC7: attention outside the filter (U3)

- In the toolbar after the switch: when the filter is on and
  `outside = all.needs_attention().count() - target.needs_attention().count()`
  is > 0, a caption 「絞り込み外に要対応{outside}件」 and a small button
  「絞り込みを解除」 that calls `clear_filter` (`mod.rs:1771-1776`). Absent
  otherwise. It replaces `queue::hidden_text` (which counted decisions only).
- Tests: filter `label:docs` (no attention match) shows "絞り込み外に要対応4件"
  over the canned fleet; clicking the button empties the filter and the
  notice goes; with no filter, or a filter that keeps every attention worker,
  no such caption exists.

### AC8: primary action (U3)

| Status | Button | Does |
| --- | --- | --- |
| Approval | 承認画面へ | `open(pane)` (focus the pane) and select it; no direct approve, the send path stays refused |
| Input | 入力する | select, open the drawer and focus its send field (the `StartSend` arm, `mod.rs:2215-2240`, extracted to `start_send(pane)`) |
| Failed | 確認する | `set_detail(Some(pane))` |
| Running | 詳細 | `set_detail(Some(pane))` |
| Ended, `result.is_some()` | 結果を見る | `set_detail(Some(pane))` with `view.result = Some(pane)`, as 表示 does (`mod.rs:2647-2651`) |
| Ended, no result path | 詳細 | `set_detail(Some(pane))` |

- 回答する is not used: no field holds the question text for certain
  (`detail` is the agent's free report, `live.rs:1499-1514`); see Q2.
- "result file exists" is `result.is_some()` (a path the drawer can read);
  the canned fleet has `has_result` without a path (`fleet.rs:714`) and
  shows 詳細 with 結果あり.
- Registry: `row_actions: Vec<(WidgetId, u64, RowAction)>` handled in
  `click` (`mod.rs:2625-2685`).
- Tests: per status, the button label, and that clicking it sends
  `Command::Focus(pane)` (Approval, over a `Channel::detached()` live feed),
  focuses the drawer's send field (Input), opens the drawer (Failed,
  Running, Ended), and for an ended worker with a result path opens the
  drawer with the result read.

### AC9: drawer order (U4)

New order inside the drawer's scroll view, after the head row (mark, label,
kind):

1. 状態: status title + qualifiers, the attention reason or latest report
   (`detail`, 3 lines), wait/update (`since`), 親 (`Fleet::parent_name`).
2. 操作: the row's primary action, 開く (open workers), the send field and
   送信 (with today's guards and hints), ペインを閉じる.
3. 結果・差分: the 結果 section (`drawer.rs:919-982`) and 差分を見る.
4. `DisclosureGroup` 「指示と画面ログ」, collapsed: the task (wrapped,
   「作業内容未取得」 when empty; it is the first line only, see Findings)
   and 画面の末尾 with its コピー.
5. `DisclosureGroup` 「実行情報」, collapsed: model · effort (agents only),
   branch, cwd, pane ID, 時間 (start, since, took, 人待ち合計, phases).

The fixed footer keeps まとめて閉じる and the drawer's 閉じる. The tag row
(model/branch/閉じた/結果) under the head goes; 閉じた / 結果あり move into
section 1 as qualifiers.

- Expanded state: `View::drawer_open: [bool; 2]` (instructions, run info),
  `false` at start, set from `Toggled` in `handle` (sources registered in
  `Registry`), kept across rebuilds and worker changes in one run, not
  saved in prefs. The screen tail is still watched while collapsed (it is
  fetched for the drawer's worker, `mod.rs:1716-1735`).
- `drawer::refresh` finds its labels by child index in the 時間 column
  (`drawer.rs:987-1022`): keep the 時間 column a plain container inside the
  disclosure with the caption first, or register each label with its role
  (`Pulse::Drawer(pane, Slot)`); the second is preferred.
- Tests (`tests/details.rs`): the section captions appear in the order
  above; both disclosures are collapsed when the drawer opens and their
  children are hidden; model / branch / cwd texts are under 実行情報 only;
  expanding 実行情報, then a structural snapshot, keeps it expanded; time
  labels still tick after a time-only snapshot.
- Risk: a hidden child of a collapsed `DisclosureGroup` must not take
  clicks or focus; verify with a click at the collapsed area in a test.

### AC10: keys, prefs, 分析, docs (U3 keys/prefs, U5 分析/docs)

- U3 keys (`keymap.rs`): `1` → 現在, `2` → 要対応, `3` → 終了
  (`Action::SetScope(Scope)`); `4` and `5` unbound (they reach the tree and
  do nothing); one help row 「1–3 表示の切替（現在・要対応・終了）」
  replaces `PANELS` (`keymap.rs:97-99`). J/K/↑/↓, Enter, Space, `/`, Esc, N,
  S, X, ⇧X, D, G, T, `?` keep their bindings; N's text says 「次の要対応」 in
  list order. The help gains one note row 「1 行 = 1 ワーカーペイン（依頼の件数ではありません）」.
- U3 prefs: see the migration policy below. Test: an old v1 file
  (`"shown": {"queue": false, "running": true, …}`, `"tab": "ops"`) loads
  without error to `scope: Current`, the other values kept; an unknown
  `"scope"` is `Current`; `to_json` writes `"version": 2` with `scope` and
  no `shown`; loading and starting from an old file writes nothing until the
  view changes.
- U5 分析: the footer's tab control (`mod.rs:378-416`) goes; the list
  toolbar ends with a 「分析」 button that opens `Tab::Analytics`. In an
  analysis tab the body's first row has 「一覧へ戻る」, a segmented
  統計 / マップ / タイムライン, and the グループ control moved there from the
  header (it only orders analysis views now). `Tab::Operations.title()`
  becomes 一覧; `T` still cycles 一覧 → 統計 → マップ → タイムライン;
  `--tab=` names are unchanged.
- U5 docs: `docs/FEATURES.md` (the Agent Dashboard paragraph, line 110):
  list, row, scopes, the attention definition, row = worker pane, keys 1–3,
  分析, the new status texts; drop panels, sparkline, urgency counts, 最長.
- Tests: U3 `tests/keys.rs`: 1/2/3 set the scope and save it, 4/5 change
  nothing, digits still type into the filter field; J/K/Enter/Space/`/`
  as before. U5 `tests/tabs.rs`: 分析 opens 統計, 一覧へ戻る returns, T cycles
  four tabs, the グループ control exists only in analysis tabs.

### AC11: connection state (U2)

`health::link_text` (`health.rs:170-194`), ages by `minutes_text`:

| Health | Text |
| --- | --- |
| Demo | ダミーデータ |
| Connecting | 接続中… |
| Live | 同期済み |
| Stale | 最後の受信から {age}（古いデータ） |
| Down, had data | 切断: 最後の受信から {age} · {reason} |
| Down, no data | 切断: {reason} |

The dot colours and `keep_on_down` stay. The worker count leaves the
status (the switch shows counts). Age is from `synced_at` and ticks every
frame as today.

- Tests (`tests/health.rs`): each row of the table, including that a Live
  link older than `STALE_AFTER` reads "最後の受信から 12 秒（古いデータ）" and
  a Down link with data keeps the fleet and reads "切断: 最後の受信から …".
  The status assertions in `tests/feed.rs`, `tests/filtering.rs:268` and
  `tests/header_fit.rs:368-410` move to the new texts.
- Risk: the status slot is 140–200 px (`header.rs:24-30`); the stale text
  puts the age first so a cut keeps it.

### AC12: checks (every unit)

Each unit runs, with `CARGO_NET_OFFLINE=true`:
`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`
on `--manifest-path widgets/agent-dashboard/Cargo.toml`, and
`scripts/verify.sh`. Done = fmt and clippy pass and no baseline-passing test
fails except those the unit names as replaced. New layout tests assert
structure and heights, not widths, so they pass on Linux and macOS.

## Prefs and keys migration policy

- Read (`Prefs::from_json`): accepts version 1 and 2 files alike (the
  version is not checked, as today). `"shown"` is ignored: panel visibility
  does not map onto a scope, and starting someone in a subset because they
  once hid a panel would hide work. `"scope"` (`current` / `attention` /
  `ended`) is read when present and valid, else `Current`. Every other key
  is read as today; a damaged file still means defaults
  (`prefs.rs:131-134`).
- Write (`Prefs::to_json`): `"version": 2`, `tab, group, scope, mapAxis,
  timeScale, filter`; no `shown`.
- An old file is not rewritten at start: it is taken as saved
  (`prefs.rs:184-195`) and replaced at the first view change. A v2 file read
  by an older dashboard: `shown` missing → all panels shown, `scope` ignored
  (`prefs.rs:140-147`): downgrade-safe.
- `tab: "ops"` (and `--tab=ops`) is the list. `--group=` keeps its meaning
  (analysis views).
- Keys: `1`/`2`/`3` switch 現在 / 要対応 / 終了; `4`/`5` do nothing. In the
  filter or send field digits type, as today (`keymap.rs:257-287`).
  `--keys=SEQ` snapshots that used digits now switch scopes.

## Units

Each unit is built in its own worktree and merged in this order:
U1 and U2 in parallel, then U3, U4, U5. A file is owned by one unit, unless
a later unit that depends on it is listed as editing it too.

### U1: attention semantics (standard)

- Files: `src/fleet.rs`, `src/dashboard/filter.rs`,
  `src/dashboard/analytics.rs`, `src/dashboard/mindmap.rs`,
  `src/dashboard/tests/mod.rs` (one `mod attention;` line),
  `src/dashboard/tests/attention.rs` (new), `src/dashboard/tests/actions.rs`
  (line 34), `src/dashboard/tests/tabs.rs` (only if a hub-count assertion
  changes).
- Adds: `Worker::needs_attention`, `Status`, `Scope`, `Worker::status`,
  `Worker::in_scope`, `Fleet::parent_name`; `cwd:` / `c:` qualifier;
  consumers switched (AC3 list above).
- Done: tests for every row of the Definitions table and the two
  invariants (fleet.rs); `tests/attention.rs` for search, analytics and hub
  counts; `branch:`/`cwd:` match; AC12 checks.
- Delivers: AC2 (membership), AC3 (search, analytics, per-parent), AC4
  (cwd searchable), AC6 (status words).

### U2: connection state text (light)

- Files: `src/dashboard/health.rs`, `src/dashboard/tests/health.rs`,
  `src/dashboard/tests/feed.rs`, `src/dashboard/tests/filtering.rs`,
  `src/dashboard/tests/header_fit.rs` (status-text assertions only).
- Done: the AC11 table as tests; AC12 checks.
- Delivers: AC11.

### U3: the worker list (deep; depends on U1 and U2)

- Files: `src/dashboard/mod.rs`, `src/dashboard/list.rs` (new),
  `select.rs`, `panel.rs`, `pager.rs`, `header.rs`, `keymap.rs`, `help.rs`,
  `prefs.rs`, `style.rs`, `graph.rs` and `timeline.rs` (panel builders
  removed), `drawer.rs` (the `CLOSED_TAG` import only); deletes `cards.rs`,
  `queue.rs`, `urgency.rs`; every file under `src/dashboard/tests/` that
  uses panels (rewritten or removed), new `tests/list.rs`, `prefs.rs` tests.
- Done: the AC1, AC2 (switch), AC3 (count = rows), AC4 (row), AC5, AC6
  (render), AC7, AC8, AC10 (keys, prefs) tests above; header without
  urgency counts, 最長 or panel chips (`header::fit` drops the グループ
  caption, then the title); the implementation record lists every deleted
  or replaced test with the test that now covers it; AC12 checks.
- Delivers: AC1, AC2, AC3, AC4 (rows), AC5, AC6, AC7, AC8, AC10 (keys,
  prefs).

### U4: the drawer (standard; depends on U3)

- Files: `src/dashboard/drawer.rs`, `src/dashboard/mod.rs` (disclosure
  state, `Toggled` handling, drawer pulses), `src/fleet.rs` (none expected;
  only if `parent_name` needs a fix), `src/dashboard/tests/details.rs` and
  the drawer assertions in other test files.
- Done: AC9 tests; the drawer uses `Fleet::parent_name`; AC12 checks.
- Delivers: AC9, AC4 (実行情報).

### U5: 分析 entry, help and docs (standard; depends on U4)

- Files: `src/dashboard/mod.rs` (footer, toolbar button, analysis row),
  `src/dashboard/header.rs` (グループ control out), `src/dashboard/keymap.rs`
  (T and `/` help texts), `src/dashboard/list.rs` (the 分析 button),
  `src/main.rs` (usage text only if wording changes),
  `src/dashboard/tests/tabs.rs`, `src/dashboard/tests/header_fit.rs`,
  `src/dashboard/tests/help_overlay.rs`, `docs/FEATURES.md`, and this spec's
  implementation record.
- Done: AC10 分析 tests; help lists every binding (row = pane note, 1–3,
  `need:any` meaning); FEATURES.md updated; AC12 checks.
- Delivers: AC10 (分析, help, docs), AC12 for the whole run.

## AC → units

| AC | Units |
| --- | --- |
| AC1 | U3 |
| AC2 | U1, U3 |
| AC3 | U1, U3 |
| AC4 | U1 (search), U3 (row), U4 (実行情報) |
| AC5 | U3 |
| AC6 | U1, U3 |
| AC7 | U3 |
| AC8 | U3 |
| AC9 | U4 |
| AC10 | U3 (keys, prefs), U5 (分析, help, docs) |
| AC11 | U2 |
| AC12 | all |

## Risks

- U3 is the critical path and the largest change: 205 of the 246
  dashboard tests sit in files tied to panels (see AC1). Its worker should rewrite tests by
  behaviour (selection, paging, empty state, filter, live update) rather than
  port them one by one, and list each removed test with its replacement.
- `mod.rs` (2689 lines) is edited by U3, U4 and U5 in sequence; nothing else
  may touch it in parallel.
- The pager change (anchor on rebuild) alters a guarantee the old spec
  tested (selection paged into view on rebuild). The selection is still
  paged into view on the user's own moves (`view.reveal`).
- Header tests measure widths with platform fonts; one already fails on
  Linux at the baseline. Replacement tests must derive widths from the
  `TextStore` they run with.
- Notice workers (`attention` flag) lose their only visible mark (the queue's
  参考情報 card). See Q4.

## Open questions (with the default taken)

1. **Pager or `ScrollView` for the list?** The goal says "one scroll
   container". Default: keep the pager (whole rows, the existing wheel and
   reveal logic, `view.first` survives rebuilds once anchored); revisit only
   if a person at the window finds paging wrong.
2. **When to say 回答する?** Default: always 入力する; no field is known to
   hold the question text.
3. **cwd in plain search terms?** Default: no; `cwd:` / `c:` qualifier only,
   so `noa` does not match every worker.
4. **Show notices (`attention` flag) at all?** Default: no row mark;
   `need:notice` still finds them. Safer than inventing emphasis the goal
   forbids for non-attention states.
5. **終了 order?** Default: open ended in fleet order, then closed newest
   ended first. A true "most recently ended first" needs an end timestamp
   `Worker` does not carry for open workers.
6. **Persist the scope?** Default: yes, like the tab; a saved 要対応 shows
   its own count and 現在 is one key away.
7. **Group headers in the list?** Default: none; rows say their parent, and
   running rows are ordered by parent. The グループ control then only orders
   the analysis views (moved there in U5).
8. **Where a selected worker went when it leaves the scope?** Default:
   deselect with a notice naming the scope it moved to; the drawer stays if
   open.

## Not verified

- Visual results at 1440×900 and 1100×700, readability of the 14 / 12 px
  rows, colours of emphasis: need a person at a window.
- `DisclosureGroup`'s handling of clicks and focus on hidden children
  (read from source only, not run).
- Whether Noa gives out pane ids in creation order (the fallback tie-break
  uses pane ids; ordering never depends on it alone).

## Implementation record

### U3: the worker list (2026-11-26, `orch/u3`)

Built as designed, with these choices and deviations:

- `list.rs` holds `rows` (membership + order), `counts`, the row texts and
  `build`; `fill_body` builds only it for 一覧. `cards.rs`, `queue.rs`,
  `urgency.rs` and the panel builders in `graph.rs` / `timeline.rs` are
  deleted; `graph.rs` keeps `mark` and `edge`, `timeline.rs` keeps what
  `gantt` measures lanes by. Code only the panels used went with them:
  `Need::{title, rank}`, `Tier::title`, `Worker::card_tail`,
  `FRESHNESS_MAX` (`fleet.rs`), `HINT_STRIP_SHRINK`, `set_tag`
  (`style.rs`). U1's `dead_code` allows are all removed: the list uses
  `Status`, `Scope`, `status`, `in_scope` and `parent_name` (the row's
  親; U4 reuses it in the drawer).
- The test-only `Dashboard::panels` is gone without a `list` field of its
  own: `Dashboard` derefs to `Registry` in the tests, whose `list` already
  names the rows; the panel is `tree.parent(list)`.
- The pager pages one list (`ListParts { list, items: (row, pane),
  indicator }`, no group headers or nested panes). A fresh list starts at
  `View::anchor` (the first row's pane, written by every fit) with
  `Aim::Stay`, or at the selection a user move asked for (`view.reveal`).
  A scope switch, a filter change and another server clear the anchor:
  those start from the top with the selection in view, as before.
- J / K and N walk the rows as shown (`walk_order`: the built list's
  panes), so a clock tick that would order the waits differently moves
  neither the rows nor the walk. A worker selected outside the list (a
  click in an analysis tab, a drawer opened on an ended worker while 現在
  is shown) stays selected while the filter shows it. A selection a live
  update takes out of the scope is cleared with 「… が一覧から外れたため
  選択を解除しました（… は 終了 に移りました）」; its open drawer stays.
- New attention is announced only once workers were shown (the first
  snapshot's rows are not news); it is joined to a dropped-selection
  notice with " · " when both come at once.
- 承認画面へ, 入力する (`start_send`, extracted from the S arm), 確認する,
  詳細, 結果を見る as in AC8. The time column is coloured only for a
  worker that needs attention (a closed failure says 失敗 in red, its
  time muted).
- `filter.rs`: `state:実行中` = `state:running`, `state:終了` =
  `state:finished`.
- Help: one row 「1–3 表示の切替（現在・要対応・終了）」, N says
  「次の要対応ワーカーを選ぶ（一覧の順）」, and the note
  「1 行 = 1 ワーカーペイン（依頼の件数ではありません）」 above the footer.
- Outside U3's file list, kept minimal: `fleet.rs` (the allows and the
  dead code above, the `Tier` doc), `filter.rs` (the `state:実行中` and
  `state:終了` aliases above), `live.rs` (one doc line naming
  `card_tail`; the test
  `a_watch_named_after_a_server_never_shown_heals_on_the_next_snapshot`
  now opens 3's drawer — Enter, J, Space — since only the drawer's
  worker is watched, and reads the drawer's overlay for the tail), and
  the drawer-, help- and tab-related test files where they used panels
  (`details.rs`, `help_overlay.rs`, `header_fit.rs`).
- Checks on Linux: fmt and clippy clean; `cargo test` 357 passed, 0
  failed, 2 ignored. The baseline failure
  `header_fit::the_drop_order_is_kept` is rewritten (two droppable items,
  widths measured from the `TextStore` it runs with) and passes.

Not verified here (needs a person at a window): the rows at 1440×900 and
1100×700, emphasis, and that the drawer (384 px, right) covers the rows'
time and 操作 columns while it is open.

### Deleted or replaced tests (U3)

| Test (before) | Now covered by |
| --- | --- |
| `actions::clicking_urgency_count_toggles_filter_and_clicking_lane_or_node_selects_worker` | `actions::clicking_a_row_selects_its_worker` (no urgency counts); lane clicks: `selection::clicking_a_timeline_lane_selects_its_worker` (the タイムライン tab's lanes, the selection painted on them); node clicks: `tabs::mindmap_switches_axes_and_detects_branch_conflict_and_opens_drawer_on_click` |
| `closed_workers::closed_workers_are_done_and_tagged` | `closed_workers::closed_workers_are_ended_and_say_so` |
| `closed_workers::a_closed_failure_is_shown_done_and_counted_failed` | `closed_workers::a_closed_failure_is_ended_and_counted_failed` |
| `details::the_card_details_button_opens_the_drawer` | `details::the_row_details_button_opens_the_drawer` |
| `details::the_running_cards_keep_their_height` | `list::every_row_is_row_height` |
| `details::a_done_group_title_with_a_paragraph_break_is_drawn_on_one_line` | rewritten under the same name on the タイムライン tab, which still draws group titles (`panel::group_header`); the list has none, and `details::tags_with_a_paragraph_break_are_drawn_on_one_line` covers the row's kind |
| `feed::the_demo_done_cards_keep_their_times_and_results` | `feed::the_demo_ended_rows_keep_their_times_and_results` |
| `feed::the_drawer_worker_is_watched_too` | `feed::the_drawer_worker_is_watched` (the drawer's worker only) |
| `filtering::the_filter_applies_to_all_five_panels` | `filtering::the_filter_applies_to_the_list_and_its_counts` |
| `filtering::the_timeline_counts_its_lanes` | `list::the_attention_count_is_the_rows_it_lays_out`, `filtering::a_query_of_blanks_shows_the_count` |
| `filtering::digits_type_into_the_filter_without_toggling_panels` | `filtering::digits_type_into_the_filter_without_switching_the_scope` |
| `filtering::a_query_of_blanks_says_which_panels_have_nobody` | `filtering::a_query_of_blanks_says_the_empty_scope_has_no_match` |
| `header_fit::at_1440_with_every_panel_hidden_only_the_caption_goes` | none: no chips; `header_fit::nothing_is_dropped_at_1440` |
| `header_fit::urgency_counts_count_each_need` | `list::the_switch_shows_each_scope_with_its_count`, `hints::the_header_holds_no_counts_longest_or_chips` |
| `header_fit::the_counts_ignore_the_filter` | `filtering::the_filter_applies_to_the_list_and_its_counts` (the switch counts under the filter, AC3) |
| `header_fit::a_hidden_item_comes_back_at_its_current_width` | none: the 最長 tag, the one item whose width changed while hidden, is gone; `header_fit::dropped_items_come_back_when_the_window_widens` |
| `hints::each_panel_title_shows_its_shortcut` | `list::the_switch_shows_each_scope_with_its_count`, `keys::digits_switch_the_scope_and_save_it` |
| `hints::hidden_panels_show_as_chips_in_the_header` | `hints::the_header_holds_no_counts_longest_or_chips` |
| `hints::the_longest_wait_tag_needs_a_wait_text` | `hints::the_header_holds_no_counts_longest_or_chips` |
| `hints::a_long_longest_label_leaves_room_for_the_link_status` | `hints::long_labels_leave_room_for_the_link_status` |
| `hints::with_nothing_to_show_the_panel_keys_stay_on_screen` | `hints::with_nothing_to_show_the_body_says_why` |
| `lanes::a_long_lane_label_is_cut_inside_its_column`, `lanes::lanes_reach_now_whatever_the_span` | none: the timeline panel is deleted; the タイムライン tab draws its own lanes (`gantt.rs`, `tabs::gantt_computes_concurrency_and_switches_time_scales`) |
| `layout::every_card_needing_you_shows_without_scrolling`, `layout::the_queue_fits_with_any_other_panels_hidden`, `window_sizes::panels_stay_apart_and_inside_the_window` | `list::rows_stay_inside_the_list_at_every_window`, `paging::every_page_shows_whole_rows` |
| `layout::all_panels_on_keeps_todays_layout`, `layout::a_hidden_panel_leaves_its_row_to_the_others`, `layout::each_shortcut_hides_and_restores_its_panel` | `list::the_default_view_is_one_list`, `keys::digits_switch_the_scope_and_save_it` |
| `layout::the_grouping_control_stays_reachable_at_1140` | `header_fit::the_header_fits_from_1100_to_1920` (the grouping control is never dropped) |
| `layout::the_last_segment_removes_the_groups` | `rebuild::a_grouping_change_reaches_the_next_frame` (groups order the analysis views only) |
| `paging::no_card_is_drawn_partly`, `paging::every_page_shows_whole_items` | `paging::every_page_shows_whole_rows` |
| `paging::the_running_panel_says_what_it_hides` | `paging::the_list_says_what_it_hides` |
| `paging::the_wheel_pages_one_item` | `paging::the_wheel_pages_one_row` |
| `paging::selecting_brings_the_card_into_view` | `paging::selecting_brings_the_row_into_view` |
| `paging::the_timeline_and_graph_page_too`, `paging::the_whole_queue_shows_with_any_other_panels`, `paging::showing_a_panel_again_shows_the_selected_card` | none: one list (`paging::*`) |
| `paging::one_long_wheel_event_pages_as_far_as_short_ones`, `paging::travel_past_the_end_does_not_eat_the_way_back`, `paging::a_long_wheel_event_stops_at_the_last_page`, `paging::a_larger_window_pages_back_what_fits`, `paging::an_unchanged_frame_presents_nothing`, `paging::a_wheel_that_turns_no_page_presents_nothing`, `paging::a_wheel_after_a_selection_turns_from_its_page`, `paging::the_latest_of_a_selection_and_a_wheel_decides_the_page` | rewritten on the list under the same names |
| `paging::clearing_the_filter_keeps_the_selected_card_in_view` | `paging::a_filter_change_starts_from_the_top_with_the_selection_in_view` |
| `paging::another_server_starts_the_lists_at_the_top` | `paging::another_server_starts_the_list_at_the_top` |
| `paging::the_wheel_does_not_fight_the_selection` | rewritten on the list under the same name: a time-only live update (the rows' times, in place) after the wheel turned the page away from the selection does not page back to it |
| `paging::a_new_size_keeps_a_page_turned_away_from_the_selection` | `paging::the_latest_of_a_selection_and_a_wheel_decides_the_page` |
| `paging::a_larger_window_shows_everything_when_an_earlier_page_fits`, `paging::more_room_finds_the_earliest_page_that_shows_the_rest` | `paging::a_larger_window_pages_back_what_fits` |
| `paging::a_card_its_list_can_hold_is_not_pushed_out_by_its_title`, `paging::a_row_taller_than_its_list_is_cut_at_the_list`, `paging::an_item_above_that_shrinks_comes_back` | none: no group titles, and every row is `ROW_HEIGHT` (`list::every_row_is_row_height`); the rebuild guarantee is `paging::a_live_rebuild_keeps_the_first_row` |
| `paging::a_shown_selection_stays_shown_when_a_card_grows` | `paging::a_shown_selection_stays_shown_when_the_window_shrinks` (a row does not grow; a smaller window pushes the shown selection below the page the same way, and the page follows it) |
| `paragraphs::a_fleet_with_paragraph_breaks_draws_in_every_panel` | `paragraphs::a_fleet_with_paragraph_breaks_draws_in_the_list_and_the_drawer` |
| `queue_cards::the_most_urgent_card_starts_expanded`, `queue_cards::details_expands_that_card_instead` | none: no expanded card; `list::the_order_is_attention_first_then_running_by_parent` |
| `queue_cards::open_names_the_pane_it_would_go_to` | `list::the_approval_button_focuses_the_pane` |
| `queue_cards::a_plain_command_has_no_model_tag` | `list::a_row_shows_its_columns_and_nothing_else` (no model in any row) |
| `queue_cards::the_queue_card_keeps_its_buttons_inside`, `queue_cards::a_long_label_leaves_the_wait_inside_its_queue_card` | `list::a_long_label_leaves_the_wait_and_the_button_inside_the_row` |
| `rebuild::hide_and_restore_cycles_release_the_old_text_layouts` | `rebuild::scope_switch_cycles_release_the_old_text_layouts` |
| `rebuild::a_press_right_after_a_panel_toggle_hits_the_grouping_control` | `rebuild::a_press_right_after_a_scope_switch_hits_the_grouping_control` |
| `rebuild::a_panel_toggle_reaches_the_next_frame` | `rebuild::a_scope_switch_reaches_the_next_frame` |
| `relations::the_orchestrator_is_the_graph_root_not_its_own_child`, `relations::the_graph_keeps_roots_with_one_name_apart`, `selection::every_graph_node_names_its_root_row` | none: the graph panel is deleted; the マップ tab draws relations (`mindmap.rs`, `tabs::*`), and a row names its parent (`list::a_row_shows_its_columns_and_nothing_else`) |
| `selection::j_and_k_walk_the_panel_order` | `selection::j_and_k_walk_the_list_order` |
| `selection::an_open_info_section_walks_its_cards_after_the_others` | `selection::j_and_k_walk_the_list_order` (the quiet 13 runs in 現在) |
| `selection::the_selection_shows_in_card_lane_and_node` | `selection::the_selection_shows_in_place_on_its_row` |
| `selection::selecting_a_queue_worker_expands_its_card`, `selection::the_queue_comes_back_with_the_selected_card_expanded` | none: no expanded card |
| `selection::the_queue_cards_keep_the_order_j_walks`, `selection::a_new_top_card_is_expanded_in_place`, `triage::an_open_info_section_reorders_in_place_as_a_build_shows_it` | `selection::the_rows_keep_their_order_as_time_passes`, `list::a_clock_advance_does_not_reorder` |
| `selection::running_and_done_cards_are_named_groups` | `selection::rows_are_named_groups` |
| `triage::the_queue_puts_decisions_first_and_folds_what_only_informs` | `list::the_order_is_attention_first_then_running_by_parent`, `list::the_attention_count_is_the_rows_it_lays_out` |
| `triage::what_only_informs_is_never_expanded_on_its_own` | `triage::what_only_informs_never_needs_attention` |
| `triage::a_visible_failure_still_says_how_many_decisions_the_filter_hides` | `triage::a_visible_failure_still_says_how_many_need_attention_outside` (failures count too, AC7) |
| `triage::a_filter_that_matches_only_folded_cards_says_so` | `triage::a_filter_that_matches_only_a_quiet_worker_shows_it_running` |
| `triage::the_drawer_shows_more_of_the_screen_than_a_card` | `triage::the_drawer_shows_the_whole_screen_tail_and_a_row_none` |
| `urgency_steps::steps_follow_the_thresholds`, `urgency_steps::a_card_looks_its_step`, `urgency_steps::a_step_change_needs_no_rebuild` | none: urgency steps are deleted with the queue cards; a row's wait still updates in place (`feed::a_volatile_snapshot_updates_in_place`) |

Tests rewritten in place under their old names (assertions moved from
panels to the list): `feed::a_volatile_snapshot_updates_in_place`,
`feed::a_structural_snapshot_rebuilds_the_screen` (now also the new
attention notice), `feed::a_live_screen_follows_its_feed`,
`feed::unknown_times_and_results_are_not_shown`,
`selection::a_vanished_selection_moves_to_its_neighbour`,
`selection::the_drawer_follows_any_reselection`,
`selection::a_reselection_to_nobody_closes_the_drawer`,
`hardening::another_server_instance_is_watched_afresh`,
`header_fit::the_drop_order_is_kept` (the baseline failure),
`header_fit::dropped_items_come_back_when_the_window_widens`,
`saved_view::*`, `keys::*` and others that used panel keys or
`queue_buttons`.

New tests (U3): `list::*` (20), `keys::digits_switch_the_scope_and_save_it`,
`keys::the_list_keys_act_on_the_rows`,
`prefs::tests::{a_version_1_file_loads_in_the_current_scope,
a_view_is_written_as_version_2_with_its_scope,
an_old_file_is_rewritten_only_when_the_view_changes}`,
`saved_view::an_old_saved_view_starts_in_the_current_scope`,
`selection::a_selection_that_leaves_the_scope_says_where_it_went`,
`paging::{a_scope_switch_starts_from_the_top, a_live_rebuild_keeps_the_first_row}`.

Review fixes (U3): 入力する clicked while a close waited for its second
press (X or ⇧X) left nothing focused — the drawer was relabelled after
`start_send` had focused its send field, and the rebuilt drawer did not
focus it again. A row's button now has the drawer relabelled before it
acts, as a key does (`act`); the drawer's own buttons are still relabelled
after. 入力する on a worker whose drawer stayed open after a live update
took it out of the list (and the selection with it) now selects it too.
Tests: `list::the_input_button_focuses_the_send_field_while_a_close_waits`,
`list::the_input_button_selects_a_row_whose_drawer_stayed_open` (an open
drawer covers the rows' 操作 column from the pointer, so with the drawer
open they press the button as assistive technology does). Checks on
Linux after the fixes: fmt and clippy clean; `cargo test` 363 passed, 0
failed, 2 ignored. Second review: a selection outside the scope's rows
that a live update drops is named in the notice (its label is now taken
from the fleet before the snapshot, not from the scope's rows), and every
switch (scope, grouping, tab, map axis, timeline scale) takes back a close
waiting for its second press, as keys 1–3 do; tests
`selection::a_selection_outside_the_list_that_leaves_says_so`,
`actions::a_switch_takes_back_a_waiting_close`; `cargo test` 365 passed,
0 failed, 2 ignored.
Third review: a click on a row (or a lane or a マップ node) away from its
button clears the focus, as a click on space that takes none does, so the
filter or send field no longer keeps it, and resets `pressed_in_filter`;
tests `filtering::clicking_a_row_leaves_the_filter_field`,
`list::clicking_the_row_leaves_the_send_field`; `cargo test` 367 passed, 0
failed, 2 ignored.
