# Agent dashboard: keyboard, filter, paging, health and details

- slug: agent-dashboard-improve
- status: implemented (2026-11-22)
- owner: simota
- scope: `mock/agent-dashboard/` (a standalone crate with its own
  `[workspace]`) and `docs/`. No change to Noa's workspace crates, `vendor/`,
  `scripts/`, CI, or any `Cargo.toml` dependency list / `Cargo.lock`.
- builds on: `docs/specs/agent-dashboard-mock.md` (panel toggles, live data).
  Its conventions hold here: headless tests through `Dashboard::dispatch` and
  `prepare`, measurements with `TextStore::new(Default::default())` at
  1440×900, Japanese UI text.

## Goal

"Improve the agent dashboard thoroughly: features, UI/UX, visibility." Ten
acceptance criteria (AC1–AC10, restated with their concrete form under
Approach). In one line each:

| AC | What |
| --- | --- |
| AC1 | Keyboard selection (j/k, ↑/↓), Enter opens the pane; selection shown in card, lane and graph node at once |
| AC2 | `/` text filter over label, agent, model, branch, task; all five panels; query + match count in the header; Esc clears; digits type, not toggle, while typing |
| AC3 | Nothing cut off: overflowing panels page and say how much is hidden; no card drawn partly; the queue's demo guarantee stays |
| AC4 | 1100×700 … 1920×1200: no panel overlaps another or leaves the window; the header does not overflow |
| AC5 | `?` shows every shortcut; Esc closes it |
| AC6 | Link state and time since last sync always visible with a colour cue; stale data marked |
| AC7 | Per-need counts visible; a waiting card is emphasised in steps as its wait passes thresholds |
| AC8 | Any worker (also 作業中 / 完了) can be opened to show task, screen tail, times, result |
| AC9 | Panel toggles 1–5, grouping control, `--demo`, `--snapshot`, `--group` stay; docs updated |
| AC10 | `scripts/verify.sh --full` green, clippy `-D warnings` clean, no new dependency, no `Cargo.lock` change |

Out of scope: sending approve/deny keystrokes to agents; changes to
`scripts/verify.sh`, CI, `vendor/`, Cargo dependency lists; any change under
`crates/` (none is needed: see Findings, "noa-ipc").

## Findings (verified 2026-11-21 on `orch/design` at `4625ecb`)

### The code today

- `mock/agent-dashboard/src/dashboard.rs` is 2860 lines: the `Dashboard`
  screen, every panel builder, and a `mod tests` of **38** tests
  (`fleet.rs` has 5, `live.rs` 25: **68** in all, `cargo test -- --list`).
- `Dashboard::build(view, theme, feed)` builds the whole tree; every change
  that is not "time only" goes through `Dashboard::rebuilt`, which removes all
  roots **and overlays** (`self.tree.overlays()` is chained into the removal),
  builds again, lays out at the last viewport and marks the root
  `Dirty::COMPOSITE`. `View` is `Copy + Eq` (`grouping`, `expanded`,
  `shown: [bool; 5]`).
- Time-only snapshots are applied in place by `Dashboard::refresh` through
  `pulses: Vec<(WidgetId, Pulse)>`; the timeline's lanes are removed and made
  again on every refresh (`fill_lanes`), so their widget ids change.
- Panel toggles are tree shortcuts: `tree.add_shortcut(panel.shortcut(),
  hint_item)` in `build`, handled as `ShortcutPressed` in `Screen::handle`.
- The header row (`header()`): title, three tally tags (作業中/要対応/完了),
  人待ち合計 tag, 最長 tag, spacer, hint strip, status label (inside a
  shrinking slot, `Truncation::Tail`), グループ caption, grouping control.
- The five panel lists are `ScrollView`s (`card_list`, and the timeline's
  own `ScrollView`); the graph's roots column is **not** in a scroll view and
  the panel does not clip, so a large graph draws past its panel.
- Only the queue expands a card inline (`View::expanded`, `QueueButtons`).
  Running and done cards are `GroupBox::card`, which has no border or
  background setter.
- The live feed (`live.rs`) publishes a snapshot when the structure changes or
  every `PULSE_EVERY` = 5 s, and wakes the window only then. `synced_secs` is
  computed when the snapshot is sent; the header's "同期 N 秒前" is therefore
  0–5 s behind and **never ticks between snapshots**. A `Down` snapshot carries
  an empty fleet, so on a lost connection the screen drops all data (the body
  shows the empty state).
- The feed fetches a screen tail for **one** pane: `Command::Expand(Option<u64>)`
  names the expanded queue card; `Tracker::fleet(now, Option<(u64, &[String])>)`
  puts that tail on that worker only.
- noa-ipc: `CONNECT_TIMEOUT` and `CONTROL_REQUEST_TIMEOUT` are 5 s
  (`crates/noa-ipc/src/client.rs`), `next_notification` waits at most
  `NOTIFY_WAIT` = 1 s, so the feed loop never blocks longer than ~5 s. No
  noa-ipc change is needed for any AC.

### Measured (`.factory` probe test, `TextStore::new(Default::default())`)

Rects are `[x, y, w, h]` in window coordinates.

- 1440×900, header children: title `[16,13.5,116,21]`; tags 作業中 63, 要対応
  63, 完了 51, 人待ち合計 108, 最長 141 (all 20 tall); spacer **21.4** px;
  hint strip 332×24; status slot 142; グループ 44; grouping control 226.6×24.6.
  The header has ~21 px to spare: **nothing can be added to it as it is.**
- 1100×700, header: the spacer is 0, the hint strip is clipped to 155 px and
  the **status slot is 0 px wide**: the link state is invisible today at the
  smallest size AC4 names.
- Queue list (1440×900): `[22,94,380,516]`; expanded card 182.3 tall,
  collapsed cards 72.3, gap 8; the last card ends at 597.6 against a list
  bottom of 610: **12.4 px** spare. The panel title row is 16 px tall.
- Running list (1440×900): group header 15, wrap row 247 (release-0.9.7),
  group header, wrap row 255 (perf-wish) at y 395 → ends at **650**, the list
  ends at 610: perf-probe is cut by 40 px (the known visible problem).
- Timeline list (1440×900): `[22,666,954,212]`, rows 15 tall with a 5 px gap;
  the last lane ends at 881 against 878: **cut by 3 px** too.
- Done cards 198 tall; graph roots column 151 tall, legend at y 825.
- Panel rects at 1100×700: queue `[12,60,400,360]`, running `[424,60,382,360]`,
  done `[818,60,270,360]`, timeline `[12,432,634,256]`, graph
  `[658,432,430,256]`; at 1920×1200: queue `[12,60,400,860]`, running
  `[424,60,1202,860]`, done `[1638,60,270,860]`, timeline
  `[12,932,1454,256]`, graph `[1478,932,430,256]`. No panel overlaps or
  leaves the window at either size today; the running card (336 px) fits the
  382 px panel at 1100.

### noa-gpui 0.2.1 (read in `~/.cargo/registry/src/*/noa-gpui-*-0.2.1/`)

Keys and routing:

- `Screen::dispatch` is a **provided trait method**
  (`noa-gpui-platform/src/screen.rs`): `tree.dispatch(event)`, then
  `handle` for every message, then `after_input`. A screen may override it, so
  the dashboard can see every event before the tree does.
- `UiTree::dispatch_event`, `Event::KeyDown` arm (`noa-gpui-core/src/tree.rs`):
  the key goes to `key_target()` (focused widget, or a modal overlay's root);
  if unhandled: Tab/Shift-Tab move focus, arrows move inside a focus group,
  Esc dismisses a hover card or a `light_dismiss` overlay, then key
  equivalents, then **tree shortcuts** (only when no modal overlay is up).
- The platform sends `Event::KeyDown` and then, for a key that types,
  `Event::Text` (`noa-gpui-platform/src/input.rs`). `Key::Char(c)` is the
  key **without modifiers, lowercased**: `?` arrives as
  `KeyDown { Char('/'), shift }` + `Text("?")` on US and JIS layouts. IME
  commits arrive as `Text`, preedit as `Event::Composition`.
- `TextField` (`widgets/text_field.rs`, editing in `widgets/editable.rs`,
  keys in `text_edit.rs::handle_key`): emits `TextChanged(String)` on a
  committed change and `TextSubmitted` on Enter (Enter is left unhandled);
  handles ←/→, ↑/↓ (single line: caret to start/end), Home/End, Backspace,
  Delete, ⌘A/C/X/V/Z. **A plain `Key::Char` is not handled by the field**: the
  text comes as `Event::Text`. So with a focused field, `KeyDown Char('1')`
  falls through to the tree's shortcuts and **toggles panel 1 while it also
  types "1"**. Esc is not handled by the field either. While a composition is
  in progress the field swallows every `KeyDown`. `TextField::text()`,
  `caret()`, `selection()`, `composition()`, `set_text()`, `select()` are
  public.
- `Modifiers::on_apple()` is "no keys held, Cmd is the command key";
  `Shortcut::matches` treats it like `Modifiers::default()`. `Key::Space`,
  `Key::Enter`, `Key::Escape`, arrows are named keys.
- `Button` activates on Space when focused (Enter only for a default button);
  `SegmentedControl` handles Space and arrows when focused. A pointer press
  focuses the nearest focusable ancestor, so a clicked 開く stays focused.

Overlays, timers, layout:

- `UiTree::open_overlay(widget, Placement, OverlayOptions) -> WidgetId`,
  `close_overlay(id, text)`. `OverlayOptions::MODAL` blocks input below and
  tree shortcuts; `OverlayOptions::PASSIVE` neither dismisses nor blocks.
  `Placement::Center` (a third of the way down) and
  `Placement::Drawer { edge: Edge::Right, top }` (laid out in the window area
  below `top`; percentage sizes refer to that area) exist.
- `UiTree::set_hidden(id, bool)`: a hidden widget **takes no space**, is not
  drawn, hit or focused. `UiTree::set_focus(Some(id) | None)`,
  `UiTree::focused()`.
- `Container::set_background(cx, Option<Paint>)` and
  `set_border(cx, Option<(f32, Paint)>)` change paint only (the border is
  painted, not laid out). `Label::set_text`, `Label::set_color`,
  `Tag::set_text` exist. `GroupBox` has no setters.
- Layout `Style` supports `position: Absolute` with insets
  (`noa-gpui-layout/src/lib.rs`).
- `UiTree::window_origin` subtracts scroll offsets (`child_offset`), and
  `display_list()` holds window-coordinate `DrawCommand::Quad { rect, radius,
  color, border }`, so tests can check a widget's fill and border by finding
  the `Quad` at its `window_rect`. `Theme::color(Role)` resolves a role.
- `Event::Wheel { pos, delta }`: positive `delta.y` moves content down (toward
  the start).
- Timers exist only for widgets (`EventCx::request_timer`); the app schedules
  frames from `tree.next_timer()` and from `Waker::wake()` (→
  `Screen::woken`, then a redraw, which calls `before_frame`).
- `UiTree::view_state`/`restore_view_state` with `set_autosave_name` save and
  restore scroll positions; this spec does not need them (pages replace
  scroll views, see AC3).
- `Kbd::new(keys)` takes literal key strings; `Kbd::keys()` reads them back;
  keycaps are 20 px tall (`KEYCAP_HEIGHT`).

### What makes criteria harder than they look

1. **AC2 vs tree shortcuts.** As shown above, a focused `TextField` lets plain
   digits fall through to `add_shortcut`. The fix is one key router in
   `Dashboard::dispatch` and **no tree shortcuts at all**.
2. **AC2 vs rebuilds.** Any full rebuild recreates the `TextField`: focus,
   caret and an IME composition would be lost on every keystroke if the
   filter rebuilt the screen. Filter changes must rebuild the body only.
3. **AC3 vs scroll views.** A `ScrollView` always shows a partly visible card
   at the edge, which is exactly the reported problem. "No card drawn partly"
   needs whole-item paging, not pixel scrolling.
4. **AC3/AC4 vs the header.** The header has 21 px spare at 1440 and none at
   1100; AC2, AC6, AC7 add to it. Something must leave the header (the hint
   strip moves into the panel titles) and the rest must collapse by priority.
5. **AC3 vs the queue fit.** The queue has 12.4 px spare. A 20 px keycap in
   the panel title row costs 4 px (row 16 → 20); nothing else may be added
   above the queue list.
6. **AC6 vs wakes.** The window only redraws on a wake; without a heartbeat
   the "seconds since sync" cannot tick and staleness cannot appear.
7. **AC8 vs the feed.** The feed fetches one screen tail; a details view for
   another worker needs a second one.

## Approach

### Shared state (lands in U1)

```rust
// dashboard/mod.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct View {
    grouping: Grouping,
    expanded: Option<u64>,      // queue card shown expanded (as today)
    shown: [bool; 5],
    selected: Option<u64>,      // AC1, a pane id
    detail: Option<u64>,        // AC8, the worker in the details drawer
    help: bool,                 // AC5
    first: [usize; 5],          // AC3, first item shown per panel (pager)
    reveal: Option<u64>,        // AC3, pane the pager must bring into view
}

struct Filter { query: String, caret: usize }   // AC2; kept across rebuilds like Feed
```

`View` stays `Copy + Eq`; the query is not in `View` (a `String` would break
`Copy`) but in `Dashboard::filter`, which `rebuilt` carries over like `feed`.

`Registry` replaces the loose `pulses`/`queue_buttons`/`timeline` fields and is
what every builder writes into:

```rust
struct Registry {
    pulses: Vec<(WidgetId, Pulse)>,
    open_buttons: Vec<(WidgetId, u64)>,     // 開く (queue card, drawer)
    detail_buttons: Vec<(WidgetId, u64)>,   // running/done 詳細 (open the drawer)
    expanded: Option<u64>,
    marks: Vec<(WidgetId, u64, Mark)>,      // AC1 selection highlight targets
    lists: [Option<ListParts>; 5],          // AC3 pager input, per panel
    timeline: Option<TimelineParts>,
    filter_field: WidgetId, match_count: WidgetId,      // AC2
    health: HealthParts,                    // AC6 (dot + status label)
    drawer: Option<WidgetId>, help: Option<WidgetId>,   // overlay roots
}
enum Mark { Bar, Lane, Node { normal: Color } }
struct ListParts {
    list: WidgetId,                         // the clipped column
    items: Vec<(WidgetId, Option<u64>)>,    // pageable items in order, with their pane
    headers: Vec<(WidgetId, Vec<usize>)>,   // group header (or wrap grid) → its item indices
    indicator: WidgetId,                    // "上に N 件 · 下に M 件" label in the title row
}
```

`QueueButtons` keeps its name as a view onto `Registry` (`open`, `details`,
`expanded`) so the existing tests that read `dashboard.queue_buttons.open` and
`.details` compile unchanged: U1 keeps a `queue_buttons: QueueButtons` field
that holds only the queue's buttons (the drawer's 開く and the running/done
詳細 buttons go to separate `Registry` vectors).

`build` is split into `build_header` and `build_body`; `refill_body` removes
the body container's children (through `UiTree::remove`, releasing text
layouts), resets the body parts of `Registry`, builds the body again, lays it
out at the last viewport and marks the root `Dirty::COMPOSITE` (the same
contract as `rebuilt`).

### AC1: keyboard selection

**Order.** `select::order(fleet, view, filter) -> Vec<u64>` is the order the
NOW row shows workers: the queue's cards (its sort: need rank, then longest
wait), then running cards, then done cards, each in `grouping.split` order,
only panels that are shown, only workers that match the filter, each pane once
(first occurrence wins: a quiet or 通知 worker sits in the queue and in
running/done). When none of queue/running/done is shown, the order is the
timeline's lane order (`grouping.split` over the matching workers).

**Moves.** `Next`: no selection → first; else the next in order, staying on
the last at the end (no wrap). `Prev`: no selection → last; else previous,
staying on the first. After a snapshot or filter change, a selected pane that
is no longer in the order is replaced by the pane now at its old index
(clamped), or `None` when the order is empty.

**Queue coupling.** When the selection lands on a worker that has a queue
card, `view.expanded` becomes that pane (as if its 詳細 were clicked: one
expanded card, so the queue fit guarantee is unchanged) and the screen is
rebuilt. Clicking a queue card's 詳細 or 開く, or any card's 詳細, also
selects that worker.

**Highlight, all in place** (`Dashboard::paint_selection`, no rebuild unless
`expanded` changed):

| Where | Widget registered | Selected | Not selected |
| --- | --- | --- | --- |
| queue / running / done card | `Mark::Bar`: a `Container` added as the card's **last** child, `position: Absolute`, inset left 0, top 8, bottom 8, width 4, radius 2, background `Role::Accent` | shown | `set_hidden(true)` |
| timeline lane | `Mark::Lane`: the lane's row container | `set_background(Role::Selection)`, radius 3 | `set_background(None)` |
| graph node | `Mark::Node`: the node pill | `set_border((2.0, Role::Accent))` | `set_border((1.0, normal))` |

The bar is absolute, so it changes no layout (the queue fit stays). Running and
done cards become `Container`s that copy `GroupBox::card`'s look exactly
(`Surface` fill, 1 px `Separator` border, radius 10; padding 18; a 14 px
weight-600 title label followed by a 10 px gap, then the body column with a
14 px gap; width 336 for running, 100 % for done), so the bar can be their
child and later units can give them buttons. Card heights must stay within
±0.5 px of the `GroupBox` ones (198 for the demo's done cards).

**Enter.** `Dashboard::open(pane)` is the existing 開く code moved into a
method, unchanged: not live → notice "ペインへ移動できません: Noa に接続していません",
no control scope → the server-scopes notice, live → `Command::Focus(pane)` and
"{label} のペインへ移動します", demo → "モック: {label} のペインへ移動します". Enter
with no selection sets the status to "ワーカーが選ばれていません（J / K で選ぶ）"
and sends nothing.

### Key map (one router for every key; lands in U1, `dashboard/keymap.rs`)

`Dashboard` overrides `Screen::dispatch`:

```rust
fn dispatch(&mut self, event: Event, text: &mut TextStore) {
    match keymap::route(self.mode(), self.composing(), &event) {
        Route::Act(action) => self.act(action, text),
        Route::Swallow => {}
        Route::Tree => {
            if !self.wheel(&event, text) {           // AC3, see pager
                self.tree.dispatch(event, text);
                for m in self.tree.take_messages() { self.handle(m, text); }
            }
        }
    }
    self.after_input();
}
```

`mode()` is `Help` when `view.help`, else `Filter` when `tree.focused() ==
Some(filter_field)`, else `Browse`. `composing()` is the filter field's
`composition()` being non-empty. `UiTree::add_shortcut` is no longer called
anywhere; `ShortcutPressed` handling is removed.

The table `keymap::BINDINGS: &[Binding]` is the single source for routing and
for the help (AC5). `Binding { mode, trigger, keys: &[&str], action, title }`.
A `Trigger::Key(k)` matches `KeyDown { key: k, modifiers }` only when shift,
alt, ctrl and command are all up (`Modifiers::default()` and
`Modifiers::on_apple()` both match). A `Trigger::Text(s)` matches
`Event::Text(s)` whatever the modifiers (`?` needs shift).

| Mode | Trigger | Keycaps shown | Action | Help title |
| --- | --- | --- | --- | --- |
| Browse | Key `Char('j')`, Key `ArrowDown` | `J`, `↓` | Next | 次のワーカーを選ぶ |
| Browse | Key `Char('k')`, Key `ArrowUp` | `K`, `↑` | Prev | 前のワーカーを選ぶ |
| Browse | Key `Enter` | `↩` | Open | 選んだワーカーのペインへ移動 |
| Browse | Key `Space` | `Space` | ToggleDetail | 選んだワーカーの詳細を開く・閉じる |
| Browse | Text `/` | `/` | StartFilter | 絞り込む（ラベル・エージェント・モデル・ブランチ・依頼） |
| Browse | Key `Escape` | `Esc` | Back | 詳細を閉じる → 絞り込みを消す → 選択を外す |
| Browse | Key `Char('1')`…`Char('5')` | `1`…`5` | TogglePanel(p) | {panel title} の表示を切り替え |
| Browse | Text `?` | `?` | OpenHelp | このヘルプ |
| Filter | Key `Escape` | `Esc` | ClearFilter | 絞り込みを消して入力を終える |
| Filter | Key `Enter` | `↩` | CommitFilter | 絞り込みを残して入力を終える |
| Filter | Key `ArrowDown` / `ArrowUp` | `↓`, `↑` | Next / Prev | 入力したまま選択を動かす |
| Help | Key `Escape`, Text `?` | `Esc`, `?` | CloseHelp | ヘルプを閉じる |

Two triggers on one row (j and ↓) are two `Binding`s sharing a title; the help
groups bindings with the same mode and title into one row.

Routing rules beyond the table:

- `Filter` with a composition in progress: **every** event is `Route::Tree`
  (the IME owns the keys).
- `Filter`, anything not in the table (letters, digits, `/`, `?`, Backspace,
  ←/→, Text) is `Route::Tree`: it edits the field. Digits therefore type and
  never toggle.
- `Help`: anything not in the table is `Route::Swallow` (keys and text do not
  reach the screen below); pointer events are `Route::Tree` (the modal overlay
  takes them).
- `Browse`, anything not in the table is `Route::Tree` (Tab focus moves,
  pointer events, wheel). Table keys win over a focused button or segmented
  control: Space and arrows act on the selection, not on a clicked control.
  After any `Act`, `tree.set_focus(None)` unless the action focuses the field
  (`StartFilter`) or is a `Next`/`Prev` from the field (Filter mode): there
  the field keeps focus, so typing goes on after ↓/↑.
- `StartFilter` is triggered by the **Text** event, so the `/` is never typed
  into the field it focuses. It sets focus to the field and the caret to the
  end of the existing query.

`Back` cascade in Browse: drawer open → close it; else query non-empty → clear
it; else selection → clear it; else nothing.

### AC2: filter

- Field: in the header, a `TextField::new("絞り込み（/）", 160.0)` followed by a
  count label (12 px, `Role::Muted`) "{matching} / {total} 件", hidden
  (`set_hidden`) while the query is empty. The field is always present, so a
  click into it also starts filtering.
- Matching (`filter::matches(worker, query)`): the query is split on
  `char::is_whitespace` (ASCII space and U+3000); a worker matches when every
  term, lowercased, is a substring of at least one of `label`, `kind`, `model`,
  `branch`, `task`, lowercased. An empty query matches all.
- `TextChanged` from the field → `filter.query`/`caret` updated →
  `refill_body` (the header and the field stay, so focus, caret and IME state
  survive) → count label updated in place. Structural snapshots that arrive
  while `composing()` are kept and applied when the composition ends (next
  `before_frame` without a composition).
- Effect per panel: queue, running, done and timeline show only matching
  workers; their title badges count the matching ones; the timeline span is
  taken over the matching workers. The graph keeps a root only when it or a
  descendant matches, keeps non-matching nodes that lie on a path to a match
  but draws them muted (label `Role::Muted`, border `Role::Outline`), and drops
  the rest. A panel with nothing matching shows a caption "該当なし". Header
  urgency counts (AC7) stay over all workers: urgency is not filtered.
- A full rebuild (panel toggle, grouping, structural snapshot) creates a new
  field with `set_text(query)`, `select(caret..caret)`, and re-focuses it when
  the old one was focused.

### AC3: paging instead of scrolling (`dashboard/pager.rs`)

- Every panel list (queue, running, done, timeline lanes, graph roots) is a
  clipped `Container` column (`fill_remaining`, gap as today, `.clip(true)`),
  not a `ScrollView`. Items: queue cards; running cards (flattened over groups;
  a wrap grid is hidden when all its cards are); done cards; lane rows; graph
  root rows. Group headers are shown when any of their items is.
- `pager::fit(tree, lists, view) -> bool`, run by `Dashboard::prepare` after
  `tree.prepare` and followed by one more `tree.prepare` when it changed
  anything:
  1. clamp `view.first[p]` to `items.len().saturating_sub(1)`;
  2. hide items before `first`, show the rest;
  3. lay out; walk the shown items from `first`: the first item whose bottom is
     more than 0.5 px below the list's bottom is hidden with every later item —
     except when it is the first shown item, which stays (an item taller than
     the list is shown alone, clipped by the list);
  4. if `view.reveal` names a pane in this list and its item is before
     `first`, set `first` to it; if it is hidden after, increase `first` by one
     and repeat from 2 (at most `items.len()` times); clear `reveal` when every
     list is done;
  5. set the title-row indicator: hidden when nothing is hidden, else
     "上に {above} 件 · 下に {below} 件" with zero parts left out
     ("下に 3 件", "上に 1 件").
- Item indices are per panel; items are counted, group headers are not. An
  item is a worker's card or lane, but in the graph it is a root row: a
  parent with all its workers. So the graph's indicator counts root rows,
  and a worker cut off inside a shown row is not counted. A root row taller
  than its list (at 1100×700, a parent given 24 more workers) is shown alone
  once paged to, from the list's top, and cut at the list's bottom (step 3,
  which treats any item taller than its list so;
  `a_row_taller_than_its_list_is_cut_at_the_list`).
- `view.reveal` is set on every selection change, so the selected worker's
  card/lane/root row is shown in every panel that has it.
- Wheel: an `Event::Wheel` whose `pos` is inside a panel list's window rect is
  taken by the dashboard (not passed to the tree): `wheel_accum[p] -= delta.y`;
  every full 40 px forward moves `first[p]` +1 while something is hidden below,
  every 40 px back −1 while `first[p] > 0`. A change runs `fit` (no rebuild).
  (Changed in implementation: see Implementation record.) The pager keeps a per-list hidden widget, `PageMemory`; when a
  selection reveal and a wheel step meet, the most recent user action decides;
  a group header is shown only when one of its items is on the page, and the
  header above the page's first item is also hidden when that item would not
  fit under it; a full rebuild (also a filter change or another server)
  drops the wheel's page: each list starts at its first item and, when the
  selected worker is not on that page, pages forward until it is; a wheel
  over a list that turns no page is passed on to the widget tree; cards paged
  out are not exposed to accessibility.
- The queue guarantee: in the demo at 1440×900 nothing in the queue is hidden
  (the existing `every_card_needing_you_shows_without_scrolling` and
  `the_queue_fits_with_any_other_panels_hidden` keep passing with only
  the queue's title row grown to 20 px, i.e. ~8.4 px spare).
- `refresh` remakes the timeline lanes: it must register the new lane ids in
  `lists[Timeline]` and `marks`, then run `fit` again.

### AC4: window sizes

- Panels: the layout rules of `agent-dashboard-mock.md` stay (fixed 400/270/430
  widths, 256 px history row, 48 px header); measured above, they keep every
  panel inside the window from 1100×700 to 1920×1200. Paging (AC3) keeps every
  card inside its panel.
- Header (`header::fit`, run by `prepare` like `pager::fit`): final order
  left → right:
  `[title] [health: dot + status slot] [urgency counts] [最長 tag] [hidden-panel chips] [spacer] [filter field + count] [? keycap] [グループ caption] [grouping control]`.
  When the children's natural widths plus gaps exceed the bar's inner width,
  drop (`set_hidden`) in this order until they fit: (1) グループ caption,
  (2) the titles of the hidden-panel chips (their keycaps stay), (3) the 最長
  tag, (4) the title. Never dropped: the health dot, the status slot (min width
  140 px, `Truncation::Tail` beyond it), the urgency counts, the filter field,
  the `?` keycap, the grouping control. Estimated worst case (1100 wide, all
  panels hidden, filter active): 1131 px needed vs 1068 available before
  dropping, 1005 after step 4. The implementer records the measured widths.
  (Changed in implementation: see Implementation record.) Measured at 1100 wide: 1469.6 px needed in the worst case, 958.6
  after all four drops; the status slot is at most 200 px (`STATUS_MAX_WIDTH`).
- The hint strip leaves the header (AC9 keeps its information elsewhere): each
  shown panel's title row starts with `Kbd::new([digit])`; each hidden panel
  appears in the header as a chip (`Kbd::new([digit])` + its title, 11 px,
  `Role::Muted`, no fill), in `Panel::ALL` order. So every panel's shortcut and
  state is still on screen.
- 人待ち合計 moves from the header into the queue panel's title row (same
  `Tag`, same `Pulse::Waited`). The three tally tags leave the header (their
  numbers are the panel title badges). (Changed in implementation: see Implementation record.) The tags left in U1, not
  U2.

### AC5: help (`dashboard/help.rs`)

- `OpenHelp` sets `view.help` and opens
  `tree.open_overlay(Container, Placement::Center, OverlayOptions::MODAL)`:
  `Role::Surface`, 1 px `Separator` border, radius 10, padding 16, width 460,
  column gap 6. Content: title "キーボード操作" (14 px, 700), then three sections
  titled "一覧", "絞り込み中", "ヘルプ" (11 px, 700, Muted), each a list of rows
  `[keycaps column, 96 px] [title label, 12 px]`, one row per distinct (mode,
  title) in `BINDINGS` order, keycaps = one `Kbd::new([k])` per key string;
  then a footer row "マウス: 詳細・開く・グループはクリックでも操作できます" (11 px,
  Muted) and a `Button::new("閉じる")`.
- `CloseHelp` (Esc, `?`, the 閉じる button) clears `view.help` and closes the
  overlay. A rebuild while help is open builds it again (overlays are removed
  by `rebuilt`).
- Height at 1100×700: about 16 rows × 22 px + headings ≈ 440 px, inside the
  700 px window (the implementer checks it, see tests). (Changed in implementation: see Implementation record.) Measured:
  `[320, 45.9, 460, 562.3]`. While help is open the screen below is disabled
  and drawn dimmed, and enabled again on close.

### AC6: connection health (`dashboard/health.rs`)

- Feed timing: `Feed` gains `synced_at: Option<Instant>`, set on every Live
  snapshot to `Instant::now() - synced_secs`. `Dashboard::tick(now: Instant)`
  is called from `before_frame` with `Instant::now()`; tests call it directly.
- Heartbeat (live.rs, U1): the feed thread calls `wake()` at least once per
  second while it runs — in `follow` after each loop pass when 1 s has passed
  since the last wake, and in `pause` (waiting with `recv_timeout` of at most
  1 s). So the window redraws every second in live mode and `tick` runs.
  (Changed in implementation: see Implementation record.) A separate thread, `live-heartbeat`, wakes the window every second
  (`HEARTBEAT`); the feed thread can wait seconds on Noa and does not do it.
  One time value is read per frame (`Dashboard::frame(now)`).
- `health::Health` from `(link, synced_age, had_data)`:

| Health | When | Dot | Status text |
| --- | --- | --- | --- |
| Demo | no link | `GRAY` | "ワーカー {n} · ダミーデータ" (unchanged) |
| Connecting | `Link::Connecting` | `YELLOW` | "接続中…" (unchanged) |
| Live | Live, age < 10 s | `GREEN` | "ワーカー {n} · 同期 {age}前" (unchanged format) |
| Stale | Live, age ≥ 10 s (`STALE_AFTER`) | `ORANGE` | "古いデータ（同期 {age}前）· ワーカー {n}" |
| Down | `Link::Down(r)` | `RED` | "切断: {r}" (unchanged) when no data was ever shown; "切断: 古いデータ（{age}前）· {r}" when it was |

(Changed in implementation: see Implementation record.) The stale mark and the age come first because the label cuts its
tail inside a slot of at most 200 px. Reasons and notices are shown on one
line.

- The dot is an 8×8 `Container` (radius 4) before the status slot; `tick`
  updates its background and the status label's text in place, and the
  status label's colour (`Label::set_color`): Muted for Demo/Connecting/Live,
  `ORANGE` for Stale, `RED` for Down. A notice (`NOTICE_FOR`) still replaces
  the text for 8 s; the dot keeps showing the health.
- Down keeps data: in `apply`, a `Down` snapshot with an empty fleet keeps the
  previous fleet when it had workers (the screen shows the last data, marked
  stale); a Down with no previous data shows the empty state as today.

### AC7: urgency

- Header counts (`header.rs`, U2): four items in this order — 承認 (Approval,
  `YELLOW`), 質問 (Question, `ORANGE`), 失敗 (Failure, `RED`), 静止 (Quiet,
  `GRAY`) — each `Label` "{title} {n}" 12 px weight 700 in the need colour, or
  `Role::Muted` weight 400 when n is 0; 8 px apart. Counted over all workers by
  `Worker::need()`. 通知 (Notice) is not counted: it waits on nobody's
  decision (it still has its queue card).
- Steps (`urgency.rs`, U5): `urgency::step(worker, need) -> u8`:

| Need | step 1 at | step 2 at |
| --- | --- | --- |
| Approval, Question, Failure | `wait_minutes` ≥ 5 | ≥ 15 |
| Quiet | `wait_minutes` ≥ 10 | ≥ 30 |
| Notice | never | never |

- Queue card look by step (`Container::set_border` / `set_background`, so it
  can change in place): step 0 — 1 px need-colour border, `SurfaceAlt` fill
  (today); step 1 — 2 px border; step 2 — 3 px border and fill
  `need_color(need).with_alpha(0.16)`. The wait label text is unchanged. The
  step is applied at build and updated by `refresh` through a new
  `Pulse::QueueCard(pane)` (registered on the card by U1, handled by
  `queue::refresh_card`, which U1 adds as a no-op and U5 fills in).
- Demo: spec-review (question, 6 min) is step 1; the others are step 0.

### AC8: details for any worker (`dashboard/drawer.rs`)

- `ToggleDetail` (Space) and every running/done card's 詳細 button set
  `view.detail` to the selected/clicked worker (selecting it), or clear it when
  it already names that worker. While the drawer is open, moving the selection
  moves the drawer to the new worker. A detail worker that leaves the fleet or
  stops matching the filter closes the drawer.
- Overlay: `tree.open_overlay(Container, Placement::Drawer { edge:
  Edge::Right, top: HEADER_HEIGHT }, OverlayOptions::PASSIVE)`, width 360,
  height 100 %, `Role::Surface`, left border 1 px `Separator`, padding 14; a
  `ScrollView` inside holds the content (one item, so AC3's paging does not
  apply). Not modal: keys keep routing through the key map. (Changed in implementation: see Implementation record.) The
  border is 1 px on all four sides (`Container` has no per-side border).
- Content, top to bottom: head row (state mark symbol in its colour as in the
  graph, label 16 px 700 with `Truncation::Tail`, kind `Tag`); tags (model,
  branch, "結果あり" `Tone::Success` or "結果なし" `TagStyle::Outlined`); section
  captions (11 px Muted) "状態", "依頼", "画面の末尾", "時間" with: state title +
  `detail` (wrap, max 3 lines); `task` (wrap, max 6 lines) or "不明"; the
  screen tail in the same monospace box as the queue card, or the caption
  "まだ取得していません" (live) / "なし" (demo) when empty; time rows "開始
  {started}前" (or "不明"), "{since}" when non-empty, "所要 {took}" for a done
  worker when known, "人待ち合計 {minutes_where(waits_on_a_person)}"; one row
  per phase total (colour square as in the timeline legend + "{phase title}
  {minutes}"); buttons 開く (`Dashboard::open`) and 閉じる.
- Time labels update in place through `Pulse::Drawer(pane)` (registered on
  each time label; `drawer::refresh` recomputes that label's text).
- Running and done cards: the caption row becomes `[caption, fill, tail
  truncation] [Button::new("詳細").control_size(Small)]`. (Changed in implementation: see Implementation record.) The button
  row is 26.32 px, not 22, and long tags in the drawer are cut by a `FitTag`.
- Screen tails for two panes (live.rs, U1): `Command::Expand(Option<u64>)`
  becomes `Command::Watch(Vec<u64>)` = `[expanded queue card, detail worker]`
  without duplicates or `None`s, sent by `build` and whenever either changes.
  The feed keeps `screens: HashMap<u64, (Instant, Vec<String>)>`, fetches each
  watched worker at most every `SCREEN_EVERY`, drops unwatched ones, and
  `Tracker::fleet(now, &HashMap<u64, Vec<String>>)` puts each tail on its
  worker. (Changed in implementation: see Implementation record.) `Tracker::fleet` takes an `Option<&HashMap<..>>`; the feed keeps
  the watch list labelled with the server instance it belongs to and fetches
  only while following that instance (`Snapshot::instance`).

### AC9: what stays

- Digits 1–5 toggle panels in Browse mode with the same layout rules (now via
  the key map); the grouping control, `--demo`, `--snapshot=FILE`,
  `--group=…` behave as before. The tests that pin them
  (`each_shortcut_hides_and_restores_its_panel`, `all_panels_on_keeps_todays_layout`,
  `a_hidden_panel_leaves_its_row_to_the_others`,
  `the_queue_fits_with_any_other_panels_hidden`, `the_last_segment_removes_the_groups`,
  `the_grouping_control_stays_reachable_at_1140`) pass unedited.
- New CLI options for review (U8): `--size=WxH` (snapshot size, default
  1440x900) and `--keys=SEQ` (dispatched before the snapshot is drawn; each
  character is sent as `KeyDown { Char(c) }` + `Text(c)`, and `<Enter>`,
  `<Esc>`, `<Space>`, `<Down>`, `<Up>` as named keys). Both only affect
  `--snapshot`. `USAGE` lists them. (Changed in implementation: see Implementation record.) `--keys` sends what the platform
  sends: Space is `KeyDown(Space)` + `Text(" ")`, a shifted symbol is its
  unshifted key with shift plus the `Text` (US layout), and `KeyDown` carries
  `command_is_meta`. A `<` that does not start one of the five names is typed
  as the character `<` alone and reading goes on from the next character, so
  `/<<Enter>` types `<` into the filter and then sends Enter; a control
  character is an error. `--size` is capped: each side at most
  4096 logical px and a padded readback of at most 128 MiB at 2× (3840x2160
  is accepted, 4096x4096 is rejected).
- Docs (U7): this spec's implementation record; `docs/FEATURES.md`'s Agent
  Dashboard entry gains one sentence (keyboard selection, `/` filter, `?`
  help, details drawer, link health); `docs/specs/agent-dashboard-mock.md` gets
  a line under its header pointing here for the header and hint-strip changes.
  `docs/feature-map.md` and `README.md` do not mention the dashboard (checked
  with grep) and are not changed.

### AC10: checks

From the repo root, for every unit:

```sh
cd mock/agent-dashboard
CARGO_NET_OFFLINE=true RUSTUP_TOOLCHAIN=1.92.0 cargo fmt --check
CARGO_NET_OFFLINE=true RUSTUP_TOOLCHAIN=1.92.0 cargo test --locked
CARGO_NET_OFFLINE=true RUSTUP_TOOLCHAIN=1.92.0 cargo clippy --locked --all-targets -- -D warnings
cd ../.. && git diff --exit-code main -- mock/agent-dashboard/Cargo.toml mock/agent-dashboard/Cargo.lock
git diff --stat main -- . ':!mock/agent-dashboard' ':!docs'     # empty
scripts/verify.sh
```

Before merging the last unit: `scripts/verify.sh --full` (noa-pty and noa-ipc
tests need the sandbox off; see AGENTS.md).

## Tests

Test names are binding; each asserts what is listed. Helpers that only one
file uses live in that file; shared ones (`window`, `laid_out`, `rect`,
`center`, `click`, `press`, `live`, `labels_under`, `tags_under`, `inside`,
`frame_of`, `fresh_frame`) in `dashboard/tests/mod.rs`. New shared helpers
U1 adds there: `key(d, Key)` (dispatch `KeyDown` with no modifiers, then
`prepare`), `typed(d, &str)` (dispatch `Text`), `quad_at(d, rect) ->
Option<(Color, Border)>` (the last `DrawCommand::Quad` whose rect equals
`rect` ±0.5 in `display_list()`), `visible_items(d, Panel) -> Vec<WidgetId>`.

U1 — `tests/keys.rs`
- `the_router_follows_the_binding_table`: for every `Binding`, building its
  trigger event and calling `keymap::route(binding.mode, false, &event)` gives
  `Route::Act(binding.action)`; with shift/alt/command held a `Trigger::Key`
  gives `Route::Tree` (Browse/Filter) or `Route::Swallow` (Help).
- `composition_gives_every_key_to_the_field`: `route(Filter, true, e)` is
  `Route::Tree` for Esc, Enter, ↓, `Char('1')`.
- `help_swallows_what_it_does_not_bind`: `route(Help, false, KeyDown j)` and
  `Text("x")` are `Route::Swallow`.
- `no_tree_shortcuts_are_registered`: dispatching `KeyDown Char('1')` with the
  filter field focused leaves `panels` all `Some` (also covered in filtering).

U1 — `tests/selection.rs`
- `j_and_k_walk_the_panel_order`: demo, Orchestrator: pressing j nine times
  visits `[14, 19, 15, 16, 13, 11, 12, 17, 18]` (queue sort: rank 0 by wait
  fix-tests 2, e2e-smoke 1, migrate-cfg 0.7, then spec-review, perf-probe;
  then running api-refactor, docs-sync; then done; perf-probe only once) and stays on 18 at
  the end; k walks back; ↓/↑ do the same.
- `the_selection_shows_in_card_lane_and_node`: select docs-sync (12): its
  running-card bar is not hidden and `quad_at` its rect has the accent colour;
  its lane row's quad has `Role::Selection`; its graph node's quad has a 2 px
  accent border; every other mark is in its unselected state; selecting another
  worker restores them. No rebuild happened (the card's `WidgetId` is unchanged).
- `selecting_a_queue_worker_expands_its_card`: selecting 19 makes
  `view.expanded == Some(19)` and `queue_buttons.open == [(_, 19)]`.
- `enter_opens_the_selected_worker_like_the_open_button`: live detached
  channel: j, Enter → `Command::Focus(14)` and status "fix-tests のペインへ移動します";
  read-only snapshot → no `Focus`, the control-scope notice; no selection →
  nothing sent, status "ワーカーが選ばれていません（J / K で選ぶ）".
- `a_vanished_selection_moves_to_its_neighbour`: select 13, apply a snapshot
  without pane 13 → the pane at its old index is selected.
- `escape_backs_out_one_step_at_a_time`: with drawer open, query "a" and a
  selection: Esc closes the drawer, Esc clears the query, Esc clears the
  selection.

U1 — `tests/filtering.rs`
- `the_filter_matches_label_agent_model_branch_and_task`: `filter::matches`
  on the demo: "codex" → {13, 15, 19}; "perf" → {13}; "テスト" → {11, 14};
  "claude sonnet" → {14, 18}; "CODEX" = "codex"; "" → all 9.
- `the_filter_applies_to_all_five_panels`: `/`, type "codex": queue has
  cards for 19, 15, 13 only, in that order; running has 13 only; done is empty and shows
  "該当なし"; the timeline has 3 lanes; the graph shows perf-wish with
  perf-probe, e2e-smoke and migrate-cfg, and no release-0.9.7 root; the count
  label reads "3 / 9 件".
- `slash_focuses_the_field_without_typing_a_slash`: dispatch
  `KeyDown Char('/')` + `Text("/")` → field focused, its text is "".
- `digits_type_into_the_filter_without_toggling_panels`: `/`, then
  `KeyDown Char('1')` + `Text("1")` → all five panels present, query "1".
- `escape_clears_the_filter_and_enter_keeps_it`: type "perf", Enter → field
  not focused, query "perf", count shown; Esc → query "", every worker back,
  count hidden.
- `typing_keeps_focus_caret_and_header`: type "co" → the field's `WidgetId`
  is unchanged and still focused, caret 2.
- `a_full_rebuild_keeps_the_query_and_focus`: type "co", press grouping via
  the control → new field has text "co", is focused, caret 2.
- `a_snapshot_waits_for_the_composition`: live; focus field, dispatch
  `Composition { text: "けん" }`; send a structural snapshot; `before_frame`
  → the field `WidgetId` is unchanged; dispatch `Text("検")` then
  `before_frame` → the snapshot is applied.

U1 — `tests/hints.rs` (replaces `the_header_shows_each_shortcut` and
`the_hint_strip_fits_the_header`, whose subject — the strip — is removed; their
remaining intent is kept below)
- `each_panel_title_shows_its_shortcut`: every shown panel's title row's first
  child is a `Kbd` whose `keys()` is `[digit]`, followed by a `Label` with
  `panel.title()`.
- `hidden_panels_show_as_chips_in_the_header`: with 2 and 4 hidden, the header
  has two chips in order (keys `["2"]`/`["4"]`, titles 作業中/状態の推移); with all
  shown, none.
- `the_status_label_is_cut_inside_its_slot`: default and all-off: not cut;
  after 開く and with a 300-character status: cut, and the label stays inside
  its slot (the assertions of the old test's second loop).
- `a_long_longest_label_leaves_room_for_the_link_status`,
  `the_longest_wait_tag_needs_a_wait_text`: unchanged.

U1 — `tests/feed.rs` (existing file): `a_live_screen_follows_its_feed` changes
`Command::Expand(None)` / `Expand(Some(14))` to `Command::Watch(vec![])` /
`Watch(vec![14])` — the only edit. New:
`the_drawer_worker_is_watched_too`: open the drawer on 12 → last command
`Watch(vec![14, 12])`. In `live.rs`'s own tests:
`two_watched_panes_get_their_tails` (`Tracker::fleet` with tails for 2 and 3
puts each on its worker, no other worker has one) replaces the `Some((2,
&tail))` call in `a_panel_becomes_a_worker` with the map form.

U2 — `tests/health.rs`
- `health_follows_link_and_age`: `health::of(link, age, had_data)` for each
  row of the AC6 table, at ages 0, 9.9 and 10 s.
- `the_status_ticks_without_a_snapshot`: live snapshot with `synced_secs` 1;
  `tick(now + 3 s)` → status "ワーカー 9 · 同期 4 秒前"; `tick(now + 12 s)` →
  "古いデータ（同期 13 秒前）· ワーカー 9" exactly (the AC6 Stale text) and
  the dot's quad colour is `ORANGE`.
- `a_lost_link_keeps_the_last_data_marked_stale`: live canned, then a `Down`
  snapshot with an empty fleet → queue still has 5 cards, status starts with
  "切断: " and contains "古いデータ", dot `RED`.
- `with_no_workers_the_body_says_why` (existing) still passes with its
  assertions unchanged; its `Down` snapshot gains `instance: None`, the field
  `Snapshot` gained (see Implementation record).

U2 — `tests/header_fit.rs`
- `the_header_fits_from_1100_to_1920`: for widths 1100, 1280, 1440, 1680, 1920
  × states {default; all panels hidden; filter focused with "perf"; status
  300 characters long; Down with a 200-character reason}: shown header
  children do not overlap (±0.5), all lie in [16, width − 16], and the dot,
  status slot (width ≥ min(140, natural)), urgency counts, filter field, `?`
  keycap and grouping control are shown with width > 0.
- `nothing_is_dropped_at_1440`: default state at 1440: no header child hidden.
- `the_drop_order_is_kept`: at 1100, all hidden + filter active: the グループ
  caption and chip titles are hidden; if the title is hidden then so is the
  最長 tag.
- `urgency_counts_count_each_need`: demo → 承認 2, 質問 1, 失敗 1, 静止 1
  (labels' texts); a fleet without failures shows "失敗 0".

U3 — `tests/paging.rs`
- `no_card_is_drawn_partly`: the demo, for 1100×700, 1440×900, 1920×1200 and
  every grouping, for each panel, every shown item lies inside its list
  (±0.5). (The demo has no item taller than its list; for the graph row that
  is, see the item rule under AC3.)
- `the_running_panel_says_what_it_hides`: demo 1440×900: perf-probe's card is
  hidden, the running indicator reads "下に 1 件"; the queue indicator is
  hidden.
- `the_wheel_pages_one_item`: wheel with `delta.y = -40` over the running
  list → `first[Running] == 1`, indicator "上に 1 件" (or "上に 1 件 · 下に …"),
  perf-probe shown; `+40` → back to 0; a wheel over the header changes nothing.
  (Changed in implementation: see Implementation record.) At the measured sizes one −40 step does not show perf-probe at
  1440×900; two steps are needed.
- `selecting_brings_the_card_into_view`: select perf-probe (13) → it is shown
  in running and in the queue.
- `the_timeline_and_graph_page_too`: busy fleet (canned + 6 running) at
  1100×700: timeline and graph lists hide whole rows only and their
  indicators count them.
- `a_volatile_snapshot_updates_in_place` (existing, in `tests/feed.rs`): its
  `ScrollView` offset steps become `first[Running] = 1` via the wheel; it
  asserts `first` survives the in-place update and the frame equals a fresh
  build paged the same way. The name stays; this is the only edit.
- Existing `every_card_needing_you_shows_without_scrolling` and
  `the_queue_fits_with_any_other_panels_hidden` additionally hold unedited
  (nothing hidden in the queue).

U3 — `tests/window_sizes.rs`
- `panels_stay_apart_and_inside_the_window`: sizes 1100×700, 1100×1200,
  1440×900, 1920×700, 1920×1200 × all 32 panel on/off combinations: shown
  panels pairwise do not overlap, all inside the window.

U4 — `tests/details.rs`
- `space_opens_the_drawer_for_any_worker`: select done 17, Space → an overlay
  exists; its labels include "lint-pass", "依頼", "clippy の警告をすべて解消する",
  "所要 16 分", "結果あり"; Space again closes it.
- `the_card_details_button_opens_the_drawer`: click the 詳細 of running card
  11 → `view.detail == Some(11)`, `view.selected == Some(11)`.
- `the_drawer_follows_the_selection`: drawer on 11, press j → drawer shows 12.
- `the_drawer_shows_the_screen_tail`: live snapshot where 12 has
  `screen_tail ["$ make"]` → a monospace label "$ make" in the drawer; with an
  empty tail → "まだ取得していません".
- `a_rebuild_keeps_the_drawer_open`: drawer on 11, toggle panel 5 → the
  drawer is open on 11 again.
- `the_running_cards_keep_their_height`: running/done card heights equal
  the measured `GroupBox` heights plus the 詳細 row difference recorded by the
  implementer (the 22 px button row replaces the 15 px caption line).
  (Changed in implementation: see Implementation record.) The button row is 26.32 px: running cards 258.32 / 242.32 / 266.32,
  done cards 209.32.

U5 — `tests/urgency_steps.rs`
- `steps_follow_the_thresholds`: `urgency::step` at 4.9/5/14.9/15 min for
  Approval, Question, Failure; 9.9/10/29.9/30 for Quiet; Notice 0 at 600.
- `a_card_looks_its_step`: demo: spec-review's card quad has a 2 px border;
  fix-tests' 1 px; set e2e-smoke's wait to 20 → 3 px and fill
  `RED.with_alpha(0.16)`.
- `a_step_change_needs_no_rebuild`: apply a volatile snapshot moving
  migrate-cfg to 6 min → its card `WidgetId` is unchanged and its quad border
  is 2 px.

U6 — `tests/help_overlay.rs`
- `question_mark_opens_help_with_every_binding`: `Text("?")` → `view.help`,
  one overlay; for every distinct (mode, title) of `BINDINGS`, the overlay has
  a row whose `Kbd` keys (in order) are that group's `keys` and whose label is
  the title.
- `escape_and_question_mark_close_help`: both close it; the overlay is gone.
- `help_blocks_the_keys_below`: with help open, `1` and `j` change neither
  `shown` nor `selected`.
- `help_fits_the_smallest_window`: at 1100×700 the overlay's window rect lies
  inside the window.

U8 — in `main.rs` `#[cfg(test)] mod tests`
- `keys_parse_into_events`: `"j/co<Enter>"` → KeyDown j, Text j, KeyDown /,
  Text /, KeyDown c, Text c, KeyDown o, Text o, KeyDown Enter.
- `size_parses`: `"1100x700"` → (1100, 700); `"1100"` is an error with USAGE.

## Risks

- **Queue fit margin.** ~8.4 px spare after the 20 px title row. Any further
  row or taller widget above the queue list breaks
  `every_card_needing_you_shows_without_scrolling`. Urgency steps and the
  selection bar are paint-only for this reason.
- **Key layouts and IME.** j/k/digits are matched as `Key::Char` without
  modifiers, as the panel toggles already were; on layouts where `/` or `?`
  need other keys, the Text triggers still work. With a Japanese IME in kana
  mode, j/k/digits start a composition outside the filter field (no focused
  widget gets it) — not verified in the window, same as the existing toggles.
- **Focused controls lose Space/arrows.** A clicked 開く or segmented control
  keeps focus; the key map takes Space and arrows before them. Accepted:
  `set_focus(None)` after every dashboard action (except ↓/↑ from the filter
  field, which keeps its focus) limits the surprise.
- **Fit passes double the layout work** on frames where pages or header items
  change (one extra `prepare`). Lists are small (tens of items).
- **Heartbeat wakes** redraw once a second in live mode; `prepare` returns
  false when nothing changed, so nothing is presented. Not measured on a
  running Noa.
- **Drawer covers the done panel** (and part of running at 1100 wide). It is
  an overlay by design and closes with Esc/Space.
- **GroupBox replacement.** The Container copy must match the measured card
  heights; `the_running_cards_keep_their_height` and the queue/layout tests
  guard it. Fonts in the windowed app may differ (same caveat as before).
- **`tests/feed.rs` is edited twice**: by U1 (wave 1) and by U3 (wave 2), so
  never by two units of one wave. `dashboard/mod.rs` is edited only by U0 and
  U1; a wave-2 unit that finds it needs a `mod.rs` change has found a gap in
  U1's stubs and must stop and report it rather than edit `mod.rs`.

## Units

Merged in wave order; units in one wave are independent and touch disjoint
files. Every unit leaves `cargo test`, clippy and `cargo fmt --check` green and
must not change a test it does not own (the edits listed under Tests are the
only allowed ones). "Done" always includes the AC10 commands.

### Wave 0

**U0 `split-modules`** — tier: standard — files:
`src/dashboard.rs` (deleted), `src/dashboard/{mod.rs, style.rs, panel.rs,
header.rs, queue.rs, cards.rs, timeline.rs, graph.rs}`,
`src/dashboard/tests/{mod.rs, layout.rs, hints.rs, queue_cards.rs,
rebuild.rs, feed.rs, lanes.rs, relations.rs}`. Behaviour-preserving: code
moves verbatim, only paths and `pub(super)` visibility change; `main.rs`
(`mod dashboard;` resolves to `dashboard/mod.rs`), `fleet.rs`, `live.rs`
untouched.

| File | Gets |
| --- | --- |
| `mod.rs` | `View`, `Feed`, `NOTICE_FOR`, `Pulse`, `Dashboard` and its impl (`new`…`build`), `impl Screen`, `#[cfg(test)] mod tests;` |
| `style.rs` | `HEADER_HEIGHT`, `GUTTER`, colours, `HINT_STRIP_SHRINK`, `gap`, `fill_remaining`, `fixed_width`, `spacer`, `caption`, `set_label`, `set_tag`, `joined`, `need_color`, `phase_color` |
| `panel.rs` | `Panel` + impl, `QUEUE_WIDTH`, `DONE_WIDTH`, `GRAPH_WIDTH`, `HISTORY_HEIGHT`, `history_row`, `panel`, `panel_title`, `card_list`, `group_header`, `empty` |
| `header.rs` | `Header`, `hint_strip`, `header`, `waited_text`, `longest`, `LONGEST_LABEL_CHARS`, `longest_text` |
| `queue.rs` | `QueueButtons`, `queue`, `queue_card`, `queue_card_summary`, `queue_card_body`, `queue_since` |
| `cards.rs` | `RUNNING_CARD_WIDTH`, `Cards`, `running`, `running_card`, `done`, `done_card`, `task_line`, `detail_line`, `identity_tags`, `model_tag`, `branch_tag`, `quiet_text`, `running_caption`, `done_caption` |
| `timeline.rs` | `LANE_*`, `LANE_SHARES`, `TimelineParts`, `timeline_span`, `timeline`, `fill_lanes`, `lane`, `span_text` |
| `graph.rs` | `mark`, `graph`, `node`, `edge`, `connect` |

Tests: `tests/mod.rs` shared helpers; `layout.rs`:
every_card_needing_you_shows_without_scrolling, all_panels_on_keeps_todays_layout,
a_hidden_panel_leaves_its_row_to_the_others, the_queue_fits_with_any_other_panels_hidden,
each_shortcut_hides_and_restores_its_panel, the_grouping_control_stays_reachable_at_1140,
the_last_segment_removes_the_groups; `hints.rs`: the_header_shows_each_shortcut,
the_hint_strip_fits_the_header, a_long_longest_label_leaves_room_for_the_link_status,
the_longest_wait_tag_needs_a_wait_text; `queue_cards.rs`:
the_most_urgent_card_starts_expanded, details_expands_that_card_instead,
open_names_the_pane_it_would_go_to, the_queue_card_keeps_its_buttons_inside,
a_long_label_leaves_the_wait_inside_its_queue_card, a_plain_command_has_no_model_tag;
`rebuild.rs`: the nine rebuild tests (…_release_the_old_text_layouts ×3,
a_press_right_after_… ×2, …_reaches_the_next_frame ×3, a_rebuild_keeps_the_appearance);
`feed.rs`: a_volatile_snapshot_updates_in_place, a_structural_snapshot_rebuilds_the_screen,
with_no_workers_the_body_says_why, a_live_screen_follows_its_feed,
unknown_times_and_results_are_not_shown, the_demo_done_cards_keep_their_times_and_results,
opening_without_the_control_scope_says_why_and_sends_nothing,
a_focus_the_server_refused_replaces_the_promise_in_the_header;
`lanes.rs`: a_long_lane_label_is_cut_inside_its_column, lanes_reach_now_whatever_the_span;
`relations.rs`: the_orchestrator_is_the_graph_root_not_its_own_child,
the_graph_keeps_roots_with_one_name_apart.

Done: `cargo test --locked -- --list` prints the same 68 test names before
and after (dashboard tests are now under `dashboard::tests::<file>::`), all
pass; no test body changed (`git diff -M` shows only moves, `use` lines and
visibility); `--snapshot` of the demo is pixel-identical to `before.png`
(`cmp`).

### Wave 1

**U1 `core-keys-select-filter`** — tier: deep — delivers AC1, AC2, the
structure of AC4's header (hint strip → panel titles and chips, waited tag →
queue title), AC9 (toggles via the key map), and the plumbing every wave-2 unit
fills. Files: `src/dashboard/{mod.rs, keymap.rs (new), select.rs (new),
filter.rs (new), header.rs, panel.rs, queue.rs, cards.rs, timeline.rs,
graph.rs}`, stubs `src/dashboard/{pager.rs, health.rs, drawer.rs, help.rs,
urgency.rs}` (new), `src/live.rs`, `src/dashboard/tests/{mod.rs, hints.rs,
feed.rs, keys.rs (new), selection.rs (new), filtering.rs (new)}`, and the empty
files `tests/{health.rs, header_fit.rs, paging.rs, window_sizes.rs,
details.rs, urgency_steps.rs, help_overlay.rs}` with their `mod` lines.

What the stubs must already do, so wave 2 only fills them:
- `pager.rs`: `ListParts`, `pub(super) fn fit(..) -> bool` returning false
  (lists stay `ScrollView`s in U1; `panel::card_list` and the timeline and
  graph lists all go through it and register `ListParts`, with the indicator
  label hidden). `Dashboard::prepare` already calls `header::fit` and
  `pager::fit` and prepares again when either returns true. `dispatch` calls
  `self.wheel(&event)`, which returns false in U1.
- `health.rs`: `HealthParts { dot, status }`, `fn status(feed, now) -> String`
  (today's `Feed::status()` text, moved here; every caller in `mod.rs` goes
  through it) and `fn tick(tree, parts, feed, now)` that sets that text (what
  `Pulse::Status` does today). U1 inserts the dot with `GRAY` and never
  recolours it. `Feed::synced_at`, `Dashboard::tick` (called from
  `before_frame`) and the heartbeat in `live.rs` are U1's.
- `drawer.rs`: `fn build(tree, root, worker, feed, registry)` that inserts the
  label and the 開く/閉じる buttons; the overlay lifecycle (open on
  `view.detail`, rebuild reopens, follow selection, close on vanish, `Watch`)
  is U1's in `mod.rs`.
- `help.rs`: `fn build(tree, root)` that inserts the title and 閉じる; open,
  close and Help-mode routing are U1's.
- `urgency.rs`: empty module; `queue::refresh_card` exists and does nothing;
  `Pulse::QueueCard(pane)` is registered.

The interfaces U1 fixes, which wave-2 units implement without touching
`mod.rs` (signatures are binding; bodies in U1 are the minimal behaviour
above):

```rust
// header.rs (U2 fills)                     HeaderParts is opaque to mod.rs
pub(super) fn header(tree, root, feed, view: &View, filter: &Filter, reg: &mut Registry) -> HeaderParts;
pub(super) fn fit(tree: &mut UiTree, text: &mut TextStore, viewport: Size, parts: &HeaderParts) -> bool;
// health.rs (U2 fills)
pub(super) fn status(feed: &Feed, now: Instant) -> String;
pub(super) fn tick(tree: &mut UiTree, parts: &HeaderParts, feed: &Feed, now: Instant);
pub(super) fn keep_on_down(old: &Fleet, new: Fleet, link: &Link) -> Fleet;
// pager.rs (U3 fills)
pub(super) fn fit(tree: &mut UiTree, text: &mut TextStore, viewport: Size,
                  lists: &[Option<ListParts>; 5], view: &mut View) -> bool;
pub(super) fn wheel(tree: &UiTree, lists: &[Option<ListParts>; 5], view: &mut View,
                    accum: &mut [f32; 5], event: &Event) -> bool;   // true = taken
// drawer.rs (U4 fills)                     returns (開く, 閉じる)
pub(super) fn build(tree, root, worker: &Worker, feed: &Feed, reg: &mut Registry) -> (WidgetId, WidgetId);
pub(super) fn refresh(tree: &mut UiTree, id: WidgetId, worker: &Worker);  // Pulse::Drawer(pane)
// queue.rs (U5 fills refresh_card); cards.rs (U4 adds 詳細 → reg.detail_buttons)
pub(super) fn refresh_card(tree: &mut UiTree, id: WidgetId, worker: &Worker);  // Pulse::QueueCard(pane)
// help.rs (U6 fills)                       returns 閉じる
pub(super) fn build(tree: &mut UiTree, root: WidgetId) -> WidgetId;
```

`Registry::detail_buttons` holds only running/done 詳細 buttons (they open the
drawer); queue 詳細 buttons stay in `queue_buttons.details` (they expand the
card). U1's `handle` already routes both, plus the drawer's and help's buttons
returned above.

Done: tests in `keys.rs`, `selection.rs`, `filtering.rs`, `hints.rs` (as
listed under Tests) and the `feed.rs`/`live.rs` edits pass; all other existing
tests pass unedited; `grep -n add_shortcut mock/agent-dashboard/src` prints
nothing.

### Wave 2 (parallel; each touches only its own files)

**U2 `header-health`** — tier: standard — AC6, AC7 (counts), AC4 (header).
Files: `src/dashboard/header.rs`, `src/dashboard/health.rs`,
`src/dashboard/tests/{health.rs, header_fit.rs}`. Removes the three tally
tags, adds the dot, the urgency counts, the final order and `header::fit`;
implements `health::of`, the Stale/Down texts in `health::status`, the dot and
status colours in `health::tick`, and the Down-keeps-data rule (in
`health::keep_on_down(old, new) -> Fleet`, which U1's `apply` already calls
as a stub returning the new fleet). U2 does not edit `mod.rs`.
Done: `tests/health.rs` and `tests/header_fit.rs` pass, with every earlier
test.

**U3 `pager`** — tier: deep — AC3, AC4 (panels). Files:
`src/dashboard/pager.rs`, `src/dashboard/panel.rs`,
`src/dashboard/tests/{paging.rs, window_sizes.rs, feed.rs}` (`feed.rs`: only
`a_volatile_snapshot_updates_in_place`). `card_list` becomes the clipped
column; `fit` and `Dashboard::wheel`'s logic live in `pager.rs` (`mod.rs`
calls `pager::wheel(&mut self.view, &self.registry, &event) -> bool`, a U1
stub). Done: `paging.rs`, `window_sizes.rs` and the edited
`a_volatile_snapshot_updates_in_place` pass; `every_card_needing_you_shows_without_scrolling`
and `the_queue_fits_with_any_other_panels_hidden` pass unedited; the demo
snapshot shows "下に 1 件" in 作業中 and no cut card.

**U4 `detail-drawer`** — tier: standard — AC8. Files:
`src/dashboard/drawer.rs`, `src/dashboard/cards.rs`,
`src/dashboard/tests/details.rs`. Done: `details.rs` passes.

**U5 `urgency-steps`** — tier: light — AC7 (steps). Files:
`src/dashboard/urgency.rs`, `src/dashboard/queue.rs`,
`src/dashboard/tests/urgency_steps.rs`. Done: `urgency_steps.rs` passes and
`every_card_needing_you_shows_without_scrolling` still passes.

**U6 `help-overlay`** — tier: light — AC5. Files: `src/dashboard/help.rs`,
`src/dashboard/tests/help_overlay.rs`. Done: `help_overlay.rs` passes.

**U8 `snapshot-options`** — tier: light — AC9 (review aid). Files:
`src/main.rs`. (Changed in implementation: see Implementation record.) It also caps `--size` and sends `--keys` as the
platform does (AC9). Done: its tests pass;
`cargo run --locked -q -- --demo --size=1100x700 --keys='jj?' --snapshot=$TMPDIR/help.png`
writes a PNG.

Units added during the run (not in this plan; see the Implementation record):
`core-hardening`, `health-followup`, `integrate-drawer`, `polish-a`,
`polish-b`. (Changed in implementation: see Implementation record.)

### Wave 3

**U7 `docs`** — tier: light — AC9 (docs), AC10 (final run). Files:
`docs/specs/agent-dashboard-improve.md` (status, implementation record with
the measured header widths, card heights and queue spare reported by U2–U4),
`docs/FEATURES.md` (the Agent Dashboard entry), `docs/specs/agent-dashboard-mock.md`
(one pointer line). Done: `scripts/verify.sh --full` green (sandbox off),
clippy clean, snapshots at 1100×700, 1440×900, 1920×1200 of the demo, with
`--keys='jjj'`, `--keys='/codex<Enter>'`, `--keys='?'` and
`--keys='jjjjj<Space>'`, looked at and attached to the PR.

## AC → units

| AC | Units |
| --- | --- |
| AC1 keyboard selection | U1 (U3 keeps the selection in view) |
| AC2 filter | U1 |
| AC3 nothing cut off | U3 (U1 registers the lists) |
| AC4 window sizes | U2 (header), U3 (panels), U1 (hint strip leaves the header) |
| AC5 help | U6 (U1 routing and lifecycle) |
| AC6 connection health | U2 (U1 heartbeat, `synced_at`, `tick`) |
| AC7 urgency | U2 (counts), U5 (steps) |
| AC8 details | U4 (U1 lifecycle and `Watch`) |
| AC9 existing behaviour, docs | U0, U1, U8, U7 |
| AC10 checks | every unit; U7 runs `--full` |

## Not verified

- The windowed app: key delivery of j/k/`/`/`?` with an IME or the default
  menu, the heartbeat redraw cost, the drawer and help as seen on screen, and
  fonts other than the test `TextStore`'s. Only headless paths were read.
- Header widths after the redesign were estimates (sum of measured parts)
  when this was written; U2 measured them (see Measured values), and U7 did
  not re-measure them.
- Whether a 4 px accent bar reads clearly enough as "selected" next to the
  need-coloured borders: owner's eye on the U7 snapshots.

## Implementation record

Implemented 2026-11-22 on `factory/agent-dashboard-improve`. The dashboard
crate has 229 tests (68 before). Values below marked "measured" are the
workers' readings with `TextStore::new(Default::default())`, and U7 did not
re-measure them. Only the card heights are also pinned by a test
(`the_running_cards_keep_their_height`, ±0.5 px); no test pins the others.
Constants and texts were checked against the code.

### Units as built

U0 `split-modules`, U1 `core-keys-select-filter`, U2 `header-health`, U3
`pager`, U4 `detail-drawer`, U5 `urgency-steps`, U6 `help-overlay` and U8
`snapshot-options` were built as planned. Five units were added during the run:

| Unit | Why it was added |
| --- | --- |
| `core-hardening` | Review findings on U1: the help modal shields the background, a press can be cancelled, the watch list survives server instances, the filter count layout. |
| `health-followup` | `had_data`, a re-tick after a rebuild, the stale wording, one-line reasons. |
| `integrate-drawer` | Merge of U4 with the pager; `FitTag` for long tags. |
| `polish-a`, `polish-b` | One-line normalisation of all Noa texts, the notice timer, health of a held snapshot, the `--size` cap, pager wheel and selection composition, the help footer, stronger tests. |

### Differences from the design

- **Header.** The three tally tags left in U1, not U2. The status slot is
  140 px at least and 200 px at most (`STATUS_MIN_WIDTH`, `STATUS_MAX_WIDTH`).
- **AC6 texts.** Stale reads `古いデータ（同期 {age}前）· ワーカー {n}`; Down
  with data reads `切断: 古いデータ（{age}前）· {reason}`. The label cuts its
  tail, so the stale mark and the age come first. Reasons and notices stay on
  one line.
- **Heartbeat.** A separate thread, `live-heartbeat`, wakes the window every
  second; the feed thread can wait seconds on Noa.
- **One time per frame.** `Dashboard::frame(now)` reads the clock once;
  notices (also "…のペインへ移動します") use the notice mechanism for 8 s.
- **Watch.** The dashboard sends its watch list once per applied Live
  snapshot. The feed keeps it labelled with the server instance it belongs to
  (`watched_on`) and fetches only while following that instance; `Snapshot`
  gained `instance: Option<String>`. When it applies the first snapshot of
  another instance, the dashboard forgets selection, drawer and expansion
  (until then it still shows the old panes; see Known issues). Pane ids mean
  something only within one instance. `Tracker::fleet` takes
  `Option<&HashMap<..>>`.
- **Rebuilds** reuse the same `UiTree`, so the app's clipboard and stylesheet
  survive.
- **Cards.** Running and done cards are `Container`s copying `GroupBox::card`,
  exposed to accessibility as named groups (focus group plus label).
- **Queue.** On volatile snapshots cards are re-ordered in place and the
  expanded card is swapped in place (`queue::set_expanded`); with no explicit
  expansion the first card is the expanded one.
- **Pager (AC3).** A per-list hidden widget, `PageMemory`, holds the page. The
  most recent user action decides between a selection reveal and a wheel step.
  A group header is shown only when one of its items is on the page (in the
  demo's running list at 1440×900 the perf-wish header goes with its cards),
  and the header above the page's first item is also hidden when that item
  would not fit under it. A full rebuild (also a filter change or another
  server) drops the wheel's page: each list starts at its first item and pages
  forward to the selected worker when that worker is not on the first page.
  Paged-out cards are not exposed to accessibility. One −40 wheel step does
  not show perf-probe at 1440×900; two are needed. A graph root row (a parent with its workers) taller than its
  list is shown alone and cut at the list's bottom, and its indicator counts
  root rows, not the workers cut inside one.
- **Details (AC8).** The 詳細 button row is 26.32 px (the spec guessed 22). The
  drawer has a 1 px border on all four sides because `Container` has no
  per-side border. A `FitTag` cuts long tags.
- **Help (AC5).** While help is open the background is disabled and dimmed.
- **Snapshot options (U8).** `--keys` sends what the platform sends (Space as
  `KeyDown(Space)` + `Text(" ")`; shifted symbols as the unshifted key with
  shift plus `Text`, US layout; `KeyDown` carries `command_is_meta`). A `<`
  that does not start one of the five names is typed as `<` alone and the
  rest is read from the next character (`<<Enter>` is `<` then Enter);
  control characters are an error. `--size` is limited to 4096 logical px a
  side and a padded readback of 128 MiB at 2× (3840x2160 accepted, 4096x4096
  rejected).
- **Texts from Noa** are folded to one paragraph at the source
  (`fleet::one_line`, `Worker::on_one_line`): cosmic-text 0.19 panics when one
  string holds a paragraph separator and mixed text directions.
- **Key map** is as designed (`keymap::BINDINGS`).

### Measured values

- Header at 1440×900: title 116, dot 8, status slot 142, urgency counts 145,
  最長 141, filter 160, count label 39, `?` 20, グループ 44, grouping control
  226.6; chips 56 / 56 / 45 / 78 / 45; bar gap 10, side padding 16.
- Header width needed against available: at 1100, 1090.6 by default → 1036.0
  after dropping グループ (1068 available); worst case (all panels hidden,
  filter active) 1469.6 → 958.6 after all four drops. At 1440, 1090.6 by
  default (nothing dropped, about 317 spare); worst case 1469.6 → 1235.6.
- Queue at 1440×900: title row 20 px, list y 98..610, last card bottom 601.6,
  8.4 px spare.
- Running list at 1440×900: 702×512; the demo shows 「下に 1 件」 (perf-probe
  paged out); the timeline also shows 「下に 1 件」.
- Cards: 詳細 row 26.32 px; running cards 258.32 / 242.32 / 266.32, done cards
  209.32 (pinned by `the_running_cards_keep_their_height`: the `GroupBox`
  heights 247 / 231 / 255 and 198 plus 26.32 − 15, ±0.5 px, laid out at
  1440×1800).
- Help at 1100×700: `[320, 45.9, 460, 562.3]`.
- Stale prefix `古いデータ（同期 59 秒前）` 154 px; Down prefix
  `切断: 古いデータ（59 分前）· ` 164 px.

### Pre-existing tests replaced or edited

- `the_header_shows_each_shortcut` and `the_hint_strip_fits_the_header` were
  replaced: their subject, the header hint strip, is gone. `tests/hints.rs`
  keeps their intent (`each_panel_title_shows_its_shortcut`,
  `hidden_panels_show_as_chips_in_the_header`).
- `running_and_done_cards_are_named_groups` now lays out at 1920×1200 (the
  paged lists would otherwise hide a card).
- `a_frame_reads_the_time_once` expects the stale text.
- The `bottom` test helper moved to `tests/layout.rs`.
- `with_no_workers_the_body_says_why` builds its `Down` snapshot with
  `instance: None` (the field `Snapshot` gained); its assertions are
  unchanged. The other existing tests get the field from the shared `live`
  helper.
- As the Tests section planned: `a_live_screen_follows_its_feed` (`Watch`
  instead of `Expand`) and `a_volatile_snapshot_updates_in_place` (wheel
  paging); `a_panel_becomes_a_worker` uses the map form of `Tracker::fleet`.

### Known issues

None was met in normal use by the reviewers. MEDIUM means reachable but
unusual.

- A notice in `--demo` can stay past 8 s when its expiry falls between
  `before_frame` and the timer of the same frame.
- During an IME composition, a held Live followed by a Down shows the age of
  the last applied sync, not the held one.
- Pasting text with a paragraph separator into the filter clears its undo
  history; a pasted U+0085 is dropped by the field before the dashboard can
  turn it into a space; the caret can land before a combining mark after such
  a paste.
- `--size` is checked against fixed limits, not the selected GPU adapter's own
  texture limit.
- A long Down reason in the empty-state body wraps over several lines.
- Pager: a sub-step wheel remainder can survive a selection and wheel burst
  handled in one frame; a selection and a backward wheel step in one fit can
  skip the fill-back; the fill-back pass lays out every earlier candidate again
  for single-column lists; a wheel over a list that turns no page is passed on
  to the widget tree instead of being consumed (it needs a three-state result
  from `pager::wheel` and a small change in `Dashboard::wheel`).
- `Command::Watch` and `Command::Focus` carry no server instance. After Noa
  restarts, the screen keeps the old server's panes until the dashboard
  applies the new server's snapshot (an applied `Down` ends this sooner: a
  screen that is not live sends no `Focus`). Until then a `Focus` sent from
  the old screen (開く, or Enter outside the filter) that the feed takes
  after reconnecting reaches whatever pane the new server gave that id.
  Without an IME composition this lasts at most until the frame after the
  feed sends that snapshot. While a
  composition is in progress in the filter field, the structural snapshots
  (the `Down` and the new server's) are held until it ends, so the window has
  no bound; a click on 開く then still sends `Focus`.
- noa-gpui: a `Button` keeps its pressed state after a second mouse button's
  release (upstream).
- `Worker::since` is drawn raw by `cards.rs` and `drawer.rs` (safe on the live
  path, which builds it from numbers; only a hand-built fleet could break it).
  The two `one_line` rules differ on CR LF (`drawer::one_line` gives one
  space, `fleet::one_line` two).
- With thousands of workers at 1100 px the header cannot fit and the status
  slot shrinks to 140 px.
- Not verified in a real window: key delivery with an IME, the heartbeat's
  redraw cost, how the drawer, help and selection bar look, trackpad paging.
- Two workspace tests outside this work are load-sensitive and failed once each
  on a heavily loaded machine (passing on rerun): noa-ipc
  `attach_added_processing_latency_stays_below_one_millisecond` and noa-pty
  `reader_reuses_returned_payload_buffers`.
