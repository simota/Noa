# Agent dashboard mock: crates.io noa-gpui and panel toggles

- slug: agent-dashboard-mock
- status: implemented (2026-11-21)
- owner: simota
- scope: `mock/agent-dashboard/` only (a standalone crate with its own
  `[workspace]`; not a member of Noa's workspace)

## Goal

Two changes to the dashboard mock, and nothing else:

1. Depend on noa-gpui from crates.io instead of `../../../noa-gpui` path
   dependencies.
2. Toggle each of the five panels (要対応 queue, 作業中 running, 完了 done,
   状態の推移 timeline, 関連 relation graph) on and off with its own keyboard
   shortcut. When a panel is off, the others take its space. The screen shows
   every panel's shortcut and whether that panel is on.

Not in scope: new panels, a settings or persistence layer for the toggles,
changes to `src/fleet.rs`, `src/main.rs`, or anything outside
`mock/agent-dashboard/`.

## Findings (verified 2026-11-21)

- Today `Cargo.toml` names four path dependencies (`noa-gpui-platform` with
  feature `native`, `-core`, `-scene`, `-text`). In this worktree
  `mock/agent-dashboard/../../../noa-gpui` does not exist, and
  `cargo metadata --offline` fails with `No such file or directory`.
- The local cache `~/.cargo/registry` has noa-gpui 0.1.0 and 0.1.1 only. 0.2.0
  was fetched into a scratch `CARGO_HOME` for this spec.
- **There is no 0.2.0 API gap for the mock.** The unmodified `src/` (all three
  files), built against crates.io `=0.2.0` in a scratch copy, passed
  `cargo test` (5/5), `cargo clippy --all-targets -- -D warnings` (clean), and
  `--snapshot` (wrote a 2880×1800 PNG). No source change is needed for AC1.
- The reference checkout differs from the published 0.2.0 only by the WebView
  work after `e683349 chore: release 0.2.0` (commits `6c89399`..`fb4ab40`):
  `ui-core/src/widgets/{webview.rs,mod.rs}`, `ui-platform/src/{webview*.rs,
  app.rs,lib.rs,wake.rs,windows.rs}`. `diff -rq` reports no other differences
  in `ui-core/src` or `ui-platform/src`. `shortcut.rs`, `event.rs` and
  `tree.rs` are identical. The mock does not use WebView.
- Shortcut facilities in 0.2.0 (`noa_gpui_core`):
  - `Shortcut::key(Key)`, `Shortcut::command(char)`, `label(glyphs)` and
    `keys(glyphs)`, plus `matches(key, modifiers)`.
  - `UiTree::add_shortcut(shortcut, target)`. On an unhandled `KeyDown` that
    matches, it emits a `Message` from `target` carrying `ShortcutPressed(s)`.
    Conditions: no focused widget handled the key, `target` is in the tree, and
    no modal overlay is up (`tree.rs`, `dispatch_event`, `Event::KeyDown` arm).
  - A tree-level shortcut fires with **no focused widget**: `key_target()` is
    `None`, so the event falls through to the shortcut list.
  - `Screen::dispatch` (`noa-gpui-platform/src/screen.rs`) runs
    `tree.dispatch`, then hands every message to `Screen::handle`.
  - `widgets::Kbd::shortcut(Shortcut)` draws one keycap per key.
  - The platform reports a character key as `Key::Char(c)`, lowercased and
    without modifiers applied (`input.rs`, `character_key`).
- Measured with `TextStore::new(Default::default())` at 1440×900. These are the
  test and snapshot conditions, not the windowed app:
  - The header row has a 363.4 px free spacer.
  - A hint strip of five `[Kbd + Label]` items (Label 11 px, item gap 3, strip
    gap 8) is 312 px wide with plain-digit shortcuts, leaving 41 px. With
    `Shortcut::command` (⌘1…) it is 500 px, which does not fit. Adding a 6 px
    status dot per item makes it 357 px, which does not fit either.
  - After 開く, the status label grows from 142 to 213 px.
  - The queue's last card ends 12.4 px above the bottom of its list, for every
    grouping. So any new full-width row costs the queue fit test.
  - Today's panel rects (x, y, w, h), the same for all four groupings:
    queue 12,60,400,560; running 424,60,722,560; done 1158,60,270,560;
    timeline 12,632,974,256; graph 998,632,430,256.

## Approach

### AC1: crates.io dependencies

In `mock/agent-dashboard/Cargo.toml`:

```toml
noa-gpui-platform = { version = "0.2.0", features = ["native"] }
noa-gpui-core = "0.2.0"
noa-gpui-scene = "0.2.0"
noa-gpui-text = "0.2.0"
```

Then build so that `Cargo.lock` re-resolves. All six noa-gpui packages
(including `-layout` and `-render-wgpu`, which come in transitively) must get
`source = "registry+https://github.com/rust-lang/crates.io-index"`. No source
change is needed (see Findings).

### AC2: one shortcut per panel

- Add `enum Panel { Queue, Running, Done, Timeline, Graph }` to
  `src/dashboard.rs`, with `ALL`, `title()` (the existing Japanese titles) and
  `shortcut()`.
- Shortcuts are the digit keys **1–5**, with no modifier, in that order:
  `Shortcut::key(Key::Char('1'))` and so on. They are chosen for width: the ⌘
  variant does not fit the header (see Findings).
- `View` gains `shown: [bool; 5]`, indexed by `Panel`. It stays `Copy + Eq`.
  `Dashboard::new` turns all five on, so its signature is unchanged.
- `Dashboard::showing` builds only the panels that are shown. It records each
  built panel's container in `panels: [Option<WidgetId>; 5]` (Changed in implementation: see Implementation record.) and registers
  every shortcut with `tree.add_shortcut(panel.shortcut(), hint_item)`, where
  `hint_item` is that panel's item in the hint strip. The item is always
  present, so the target is always in the tree.
- `handle`:
  1. On `ShortcutPressed(s)`, find the panel with `shortcut() == s`, flip its
     flag, and rebuild with `*self = Self::showing(View { shown, ..self.view })`. (Changed in implementation: see Implementation record.)
     This is the pattern the grouping and 詳細 handlers already use, so the
     shortcuts are registered again on the new tree.
  2. The existing `ButtonClicked` early return must come after this check.

Layout when panels are off. The rows are `now` (queue | running | done) and
`history` (timeline | graph).

- In a row, the grow panel (running, timeline) keeps `fill_remaining()`. While
  the grow panel is shown, the fixed panels keep their fixed width.
- When a row's grow panel is off, the row's remaining panels get
  `flex_grow: 1.0` with `flex_basis` equal to their fixed width. They share the
  row, and a lone panel fills it.
- A row with no shown panel is not inserted.
- When `now` is absent, `history` takes `fill_remaining()` instead of the fixed
  `HISTORY_HEIGHT`. When `history` is absent, `now` (already
  `fill_remaining()`) takes the whole body.
- With all five off, the body is empty. The hint strip still shows all five as
  off.

### AC3: shortcuts and state on screen

- Add a hint strip to the header bar, after the spacer and before the status
  label. It is a row with `flex_shrink: 0`, gap 8, and one item per panel in
  `Panel::ALL` order. Each item is a row with gap 3 holding:
  - `Kbd::shortcut(panel.shortcut())`;
  - `Label::new(panel.title())` at 11 px.
- On is drawn with the default text color at weight 700. Off is drawn with
  `Role::Muted` at weight 400. The strip has the same width in either state, so
  toggling never moves the header. (Changed in implementation: see Implementation record.)
- Keep the header from overflowing after 開く. The status label goes inside a
  container with `min_size.width: 0` and `flex_shrink: 1`, and gets
  `Truncation::Tail`. `dashboard.status` stays the Label's id, so
  `open_names_the_pane_it_would_go_to` reads `text()` unchanged. At the default
  status text there is no truncation (142 px fits).

### AC4: existing tests and look

- The five existing tests call `Dashboard::new`, `dispatch` and `prepare`,
  whose signatures do not change. They must pass without edits.
- `src/main.rs` is not edited, and `--snapshot` keeps working.
- With all panels on, the five panel rects must equal the measured values
  above. The only addition is the hint strip, which sits in the header's spare
  width.

### AC5: checks and footprint

From the repo root:

```sh
cargo clippy --manifest-path mock/agent-dashboard/Cargo.toml --all-targets -- -D warnings
cargo test   --manifest-path mock/agent-dashboard/Cargo.toml
git diff --stat main -- . ':!mock/agent-dashboard' ':!docs/specs/agent-dashboard-mock.md'   # empty
```

`mock/agent-dashboard/target/` is ignored by `mock/agent-dashboard/.gitignore`.

## Files

| File | Unit |
| --- | --- |
| `mock/agent-dashboard/Cargo.toml` | U1 |
| `mock/agent-dashboard/Cargo.lock` | U1 |
| `mock/agent-dashboard/src/dashboard.rs` | U2, then U3 (U2 first) |

`src/fleet.rs` and `src/main.rs` are not edited.

## Risks

- **0.2.0 API gap.** None for the current mock (verified by building it). The
  new code uses only `Shortcut`, `ShortcutPressed`, `Key`, `Modifiers`,
  `UiTree::add_shortcut` and `Kbd`, and all of these are present in 0.2.0. Do
  not copy anything from the reference checkout's WebView commits.
- **Focus.** Shortcuts fire only when the focused widget does not handle the
  key. `Key::Char` is matched only by the widgets that reference it in
  `widgets/*.rs`: alert, collection, editable, find, chart, log_view, panes,
  outline, terminal, table. Button, SegmentedControl and ScrollView are not
  among them. Chart handles digits only while focused, and only for non-sparkline
  charts with a legend. The mock's charts are sparklines, which are not
  focusable. The rebuild on toggle drops any focus anyway. A future text field
  or legend chart in the mock would swallow 1–5 while focused.
- **Plain digits also type text.** The platform sends `Event::Text("1")` as well
  as the key. The mock has no text input, so this is harmless.
- **Queue fit with panels off.** The queue fit test is untouched and runs only
  with all panels on. Hiding a panel only widens or heightens the queue: it
  never adds a row and the hint strip does not grow. A new test covers every
  combination (U2). Do not add any full-width row to the body or root: the
  queue has only 12.4 px spare.
- **Header width.** It has 41 px spare at defaults in the test font. The
  windowed app's fonts may measure differently (unverified). The U3 test guards
  the header.
- **Lockfile churn.** Replacing path packages may re-resolve other
  dependencies. The size of the `Cargo.lock` diff is unverified.
- **Fetching.** `~/.cargo` has no 0.2.0. The first build needs network access
  to crates.io, and writes to `~/.cargo`, which the sandbox blocks. Run it
  unsandboxed, or with a `CARGO_HOME` under `$TMPDIR`.

## Units

Build them in order. Each unit leaves the crate green.

### U1: crates.io dependencies (AC1)

Edit `Cargo.toml` as above and let cargo update `Cargo.lock`. Before touching
`src/`, write the baseline image:
`cargo run --manifest-path mock/agent-dashboard/Cargo.toml -- --snapshot=$TMPDIR/before.png`.

Done when, in this worktree, all of the following hold:

- `grep -n 'path *=' mock/agent-dashboard/Cargo.toml` prints nothing.
- `grep -A2 'name = "noa-gpui' mock/agent-dashboard/Cargo.lock` shows version
  `0.2.0` and a registry `source` for all six packages.
- The AC5 clippy and test commands pass with 5 tests.
- The snapshot command writes a PNG.

### U2: panel model and hiding (AC2 layout, AC4)

Add `Panel`, `View::shown`, `Dashboard::panels` and the layout rules, with no
shortcuts or strip yet. New tests in `dashboard.rs` build views directly with
`Dashboard::showing`:

- `all_panels_on_keeps_todays_layout`: the five rects equal the measured values
  (±0.5 px) for every grouping.
- `a_hidden_panel_leaves_its_row_to_the_others`: for each single panel off,
  `panels[p]` is `None`. The shown panels of each remaining row start at the
  row's left edge and end at its right edge (±0.5 px). With both panels of a
  row off, the other row spans the body.
- `the_queue_fits_with_any_other_panels_hidden`: for all 16 on/off combinations
  of the other four panels × 4 groupings, every queue card ends at or above
  the list bottom (same check as the existing fit test).

Done when the AC5 commands pass: the 5 old tests unedited, plus the 3 new ones.

### U3: shortcuts and hint strip (AC2 keys, AC3)

Add `Panel::shortcut`, the hint strip, `add_shortcut` registration, the
`ShortcutPressed` handling and the status truncation. New tests:

- `each_shortcut_hides_and_restores_its_panel`: for each panel, and with both
  `Modifiers::default()` and `Modifiers::on_apple()`:
  1. Dispatch `Event::KeyDown { key: Key::Char(digit), modifiers }` through
     `dashboard.dispatch`, then `prepare`.
  2. Assert `panels[p]` is `None` and the other panels are present.
  3. Dispatch the same key again and assert the panel is back at its measured
     rect.
- `the_header_shows_each_shortcut`: in the default view and with all five off,
  the strip has five items in `Panel::ALL` order. Each item's `Kbd::keys()`
  equals `panel.shortcut().keys(tree.shortcut_glyphs())`, and its Label `text()`
  equals `panel.title()`.
- `the_hint_strip_fits_the_header`: in three states (default, all off, and
  after clicking 開く), adjacent header children do not overlap and the strip
  keeps its natural width (312 px in the test font). (Changed in implementation: see Implementation record.)

Done when all of these hold:

- The AC5 commands pass: 5 old tests unedited plus 6 new ones. (Changed in implementation: see Implementation record.)
- The `git diff` footprint check is empty.
- `--snapshot=$TMPDIR/after.png` writes a PNG that, compared by eye with
  `before.png`, differs only by the header hint strip.

## Not verified

- How the windowed app (`cargo run` without `--snapshot`) delivers plain-digit
  keys on macOS. Two cases are untested: whether winit's default menu or IME
  claims any of 1–5, and how keys behave while an IME is active. Only the
  headless dispatch path was read and tested.
- Header width with the windowed app's fonts, as opposed to the default
  `TextStore`.
- Whether off (muted, weight 400) versus on (weight 700) reads clearly enough.
  This needs the owner's eye on `after.png`. The state is not exposed through a
  widget getter (`Label` has no color or weight accessor in 0.2.0), so tests
  check the shortcut text and the layout, not the styling.

## Implementation record (2026-11-21)

- Lockfile: the path-to-registry switch (`f6a8743`) removed 619 lines and added 24 in `Cargo.lock`. It added no package other than the six noa-gpui ones.
- Hint strip look: both states use the same weight. On is a filled `SurfaceAlt` pill with the default text colour; off has no fill and `Role::Muted` text. The spec's weight 700 / 400 was dropped because weight changes the text width (300 px vs 312 px), which moved the strip when a panel was toggled. The strip is 332 px wide at 1440x900 in the test font, not 312.
- Narrow windows: the strip has `min_size.width` 0, clipping, and a very small `flex_shrink`, so at 1140 px wide it is clipped (its last items are cut) and the grouping control stays reachable. Not all five hints are readable at that width.
- Rebuilds go through `Dashboard::rebuilt`, which:
  - keeps the current theme (a light system appearance is no longer reset to dark);
  - removes the old tree's roots through `UiTree::remove`, so their text layouts are released;
  - lays the new tree out at the last viewport before returning, so input that arrives before the next frame hits the right widget;
  - then marks the new root `Dirty::COMPOSITE`, because the windowed app requests a redraw only when `needs_update` is true and presents only when `prepare` returns true.
- `Dashboard::panels` exists only in test builds (`#[cfg(test)]`); nothing in non-test code reads it.
- Tests: 21 in `dashboard.rs`; the 5 original ones are unedited.
  - Original: `every_card_needing_you_shows_without_scrolling`, `the_most_urgent_card_starts_expanded`, `details_expands_that_card_instead`, `open_names_the_pane_it_would_go_to`, `the_last_segment_removes_the_groups`.
  - Layout with hidden panels: `all_panels_on_keeps_todays_layout`, `a_hidden_panel_leaves_its_row_to_the_others`, `the_queue_fits_with_any_other_panels_hidden`.
  - Shortcuts and hint strip: `each_shortcut_hides_and_restores_its_panel`, `the_header_shows_each_shortcut`, `the_hint_strip_fits_the_header`, `the_grouping_control_stays_reachable_at_1140`.
  - Rebuild behaviour: `hide_and_restore_cycles_release_the_old_text_layouts`, `a_grouping_change_releases_the_old_text_layouts`, `a_details_click_releases_the_old_text_layouts`, `a_press_right_after_a_panel_toggle_hits_the_grouping_control`, `a_press_right_after_a_details_click_hits_the_open_button`, `a_panel_toggle_reaches_the_next_frame`, `a_grouping_change_reaches_the_next_frame`, `a_details_click_reaches_the_next_frame`, `a_rebuild_keeps_the_appearance`.
- Known limits, accepted:
  - a rebuild resets scroll positions, the header status text, hover and an in-progress pointer press;
  - a held key toggles again on each repeat;
  - modifier matching on non-Apple platforms is noa-gpui's;
  - the hint-strip test's expected widths assume the default `TextStore` system fonts.
- Not verified: the windowed app (key delivery with an IME or the default menu, fonts, and what the on/off look reads like on screen). Only the headless paths were run.

## Live data from noa-ipc (2026-11-21)

Added after U1–U3 at the owner's request. The crate shows the running Noa's
panes by default; `--demo` shows the canned fleet, which the layout tests use.
The scope line and Goal above describe the earlier, canned-only state.

- Dependencies: path dependencies on `crates/noa-ipc` and `crates/noa-config`,
  and `serde_json` (to read the journal). Noa's own workspace, lockfile and
  crates are unchanged.
- Connection (`src/live.rs`): one background thread owns a `noa_ipc::Client`.
  It finds the server as `+ctl` does (`load_startup_config`, then
  `server_token_path`) and asks for the `read` and `control` scopes only. It
  never sends input. A failed connection is retried from 0.5 s up to every 4 s
  and shown in the header with its reason.
- Calls made: `noa.subscribe` (`state_changed`), `noa.listPanels` every 2 s (a
  closed pane sends no event), `noa.getText` at most every 2 s for the one
  queue card that is expanded, and `noa.focusPane` when 開く is clicked.
  `noa.output` is not subscribed: its load on Noa was not measured.
- Which panes are shown: a panel with `agent` or `parentPaneId`, except those
  labelled `board:…`. A pane with no parent that has workers under it is an
  orchestrator and names their group; an agent pane with no parent and no
  workers goes under `単独`.
- Where each value comes from:
  - state, detail and report count: `Panel.agent` and `Panel.exit`;
  - model and effort: the flags in `Panel.spawn.argv`;
  - task: the latest non-answer `send` after the pane's last `spawn` in the
    spawner's journal (`ctl_journal_path` of the parent's `surfaceId`);
  - `結果あり`: a non-empty `result-<label>.md` (or with an 8-hex suffix) in
    `<cwd>/.noa/`;
  - start time: `Panel.spawn.startedAt`.
- What the dashboard records itself, from its own start only: the timeline
  phases, how often `Panel.preview` changed per 5 s (the sparkline and the
  freshness meter), and quiet (running with no preview change for 120 s). A
  time whose start was not observed is not shown; a wait already in place at
  first sight is shown as a lower bound (`N 分以上`).
- Updates: a snapshot that changes only times, the sparkline, freshness or the
  timeline is applied in place; one that changes the set of workers, a state,
  a group or a text rebuilds the screen through `Dashboard::rebuilt`.
- Checked on the running Noa (0.9.6) with one command worker and one `light`
  claude worker, by rendering to PNG: the fleet matched `+ctl ls`; model,
  effort, task, result tag, parent links, observed transitions with their
  durations and the expanded card's screen tail were shown.
- Not verified: the window itself (the in-place updates as the user sees them,
  and `noa.focusPane` from 開く); codex and agy workers; approval and question
  states on a real pane; a long worker label in the queue card's head row.
- Known: a worker that finished with `attention` set appears in both the queue
  (通知) and 完了.
