# Widget navigation and display

The standalone widgets share display preferences and task handoffs while keeping
their own stored sessions. Task handoffs prefill start forms or open selected
tasks and saved runs. Prefilling a form does not start agents.

## Find a widget by its purpose

The command palette searches Japanese purpose words as well as the existing
English names and descriptions:

| Search | Opens |
| --- | --- |
| 準備, 開発環境 | Scaffold |
| タスク, 期限 | Tasks |
| レビュー, 指摘 | Review |
| 検証, 動作確認, 証跡 | Verify |
| ワークフロー, 工程 | Workflow |
| 使用量, 利用量, 残量 | Agent Usage |
| ウィジェット, カタログ | Repository Widgets |
| 表示設定, 文字サイズ, 密度 | Widget Appearance Settings |

`widget-settings.open` opens the common preferences through the installed widget
host. Every widget also accepts `--widget-settings` by itself, before backend or
repository initialization.

The preferences offer system, light and dark appearance, body and input text at
100%, 115%, 130% or 150%, and standard or compact spacing. **本文・入力の文字サイズ**
scales labels, buttons and editable text. Auxiliary menus, segmented controls and
tags retain GPUI's default size; their current public API does not support an
arbitrary text scale. Compact spacing changes whitespace
without reducing the text scale or the minimum pointer targets. Save, then
reopen an already open widget to apply the saved settings. The settings window
previews the selected appearance and text size before saving.

Preferences are stored in `noa/widget-appearance.json` under the platform's data
directory. `NOA_WIDGET_APPEARANCE_FILE` can select an absolute file path for a
reproducible display. Demo and snapshot runs use their default appearance unless
this override is supplied, so a user's preferences do not silently change
reference screenshots. A damaged settings file does not prevent opening a
widget; the settings window reports the problem and can replace it on Save.

## Follow a task through its next step

Scaffold's task-creation result includes **作成したタスクを開く**, which opens Tasks
with the created task selected. Existing tasks retain the same navigation action.
In folders without Git, creating setup tasks remains available, but the Tasks
shortcut explains its Git requirement and stays disabled until a rescan finds a
Git repository.

Tasks offers **この版をレビュー** and **この版を検証** for a recorded change that
has no corresponding session. The launch carries the repository, recorded base,
full target commit, task selector and run ID. The action checks that the displayed
revision is still the run's current recorded revision before it launches the
other helper. Opening the form runs no agents.

Review and Verify accept the handoff as:

```text
--repo=ABSOLUTE_DIR --base=FULL_SHA --revision=FULL_SHA
--origin-repo=ABSOLUTE_DIR --origin-task=TASK_SELECTOR --origin-run=RUN_ID
```

Both commits must be full hexadecimal object IDs. The origin arguments are
all-or-none and require the repository and both commits. Demo and stored-session
modes cannot be combined with a task handoff.

When the person starts the session, the runner compares the captured repository
and both commits against the explicitly requested task/run, its `ChangeBase`
and `ChangeSeen` records, and the exact originating Tasks workspace.
A stale or mismatched handoff is refused. It never silently chooses a different,
newer task run just because that run has the same commit. The resulting session
is linked to the original task; the existing **元のタスクを開く** action returns to it.
A new Review or Verify form clears the previous handoff's explicit identity.

Tasks can also open Workflow with the selected task's title and objective.
Workflow records the origin and publishes bounded result references for Tasks to
show, including its run, state, target digest and PR when present. Those results
remain Workflow agent reports; they are not presented as Review or Verify widget
sessions, and do not satisfy those widgets' evidence contracts. See
[Workflow](WORKFLOW.md) for assignment presets, immutable runs and result details.

## Use smaller windows

Tasks and Review can be narrowed to 720 logical pixels. Tasks switches between
its list and selected detail. Review shows one of its existing panes at a time:
**対象・パネル**, **指摘・結果**, and **選択の詳細**. Selecting a finding reveals its
detail; choosing a center tab returns to the findings/results pane. Widening the
window restores the split panes and their prior sizes.

Changing the layout reuses the existing editor widgets. Resizing or changing
panes does not recreate a search field, discard a draft, or change the selected
item. Review's initial form and past-session cards stack vertically at a narrow
width. Verify retains its existing compact pane navigation.

Repository widget rows wrap when their contents no longer fit. Text input widths
follow their parent instead of retaining the old fixed 360-pixel width. A data
refresh that has no view change does not rebuild the body; updates retain live
text-input subtrees and restore action focus by stable component identity. See
[Repository widgets](REPOSITORY_WIDGETS.md) for action status, catalog diagnostics,
fixtures and image previews.

## Plan by due date

Tasks' calendar separates **予定**, **実績**, and **両方**. Planned entries come from
the task's due date; actual entries come from recorded run starts and finishes.
Both can be shown for the same day without treating a due date as an execution
record. Overdue unfinished tasks are marked, and selecting a calendar entry
opens its task detail. Dense days expose the remaining entries rather than
silently dropping them.

## Inspect saved images

Verify's gallery and criterion details show native PNG, JPEG and WebP thumbnails.
A single worker verifies the saved file and digest before bounded decoding; the
UI reuses a limited cache. Select an image to open a native window with image
metadata and fit/original-size controls. Unsupported media and unavailable or
invalid images retain their existing open/compare routes. The thumbnail is a
reduced sample; inspect the original-size view for pixel-level comparisons.
See [Verify image previews](VERIFY.md) for the exact limits and fallback behavior.

## Validation scope

Layout and handoff tests cover narrow panes, preserved drafts, incomplete launch
arguments, and explicit task identity when more than one run records the same
commit. Catalog and lifecycle tests exercise malformed definitions, fixture
bounds, delayed reports, duplicate requests and recovery. Visual behavior on a
real macOS or Linux desktop still needs manual review, especially Japanese IME,
large text, and a mix of long labels and small windows. A successful compilation
or headless layout test is not a claim of manual visual verification.
