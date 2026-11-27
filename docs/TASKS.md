# Move a task through its related widgets

Tasks keeps local tasks, GitHub issues, and code markers in one workspace.
Choose a task to see its description, personal properties, agent runs, and
related results. `tasks --repo=/path/to/project --select=t-123` opens a
specific local task; issue numbers use `--select=#12`.

## Review and verify a recorded change

The current change version in **関連成果** offers **この版をレビュー** and
**この版を検証** when that kind of session is not registered for the version.
Each button opens the corresponding setup form with the task's repository,
the run's recorded starting commit, the selected full commit ID, and the
original task/run filled in. Review the setup before starting an agent.

The button checks the recorded version again before opening the form. If a
new version has arrived, the starting commit is unknown, or a commit has been
removed from the repository, it shows an explanation instead of silently
checking another target. A session that belongs to an older version remains
visible as historical evidence.

## Hand work to Workflow

**Workflow で進める** opens Workflow's setup form with the selected task's
title and full loaded description. The handoff does not create a run until
the user starts one there. The objective is an immutable, private JSON file
under the Tasks data folder. Titles are limited to 4 KiB and objectives to
64 KiB; an oversized instruction produces an explanation rather than being
truncated.

Workflow records the original task and publishes bounded result summaries
back to **関連成果**. Its run can be reopened from there, and Workflow offers
a link back to the selected task. These entries are explicitly labeled as
Workflow agent reports. They do not register a Review or Verify session and
do not stand in for those widgets' decisions.

## Plan with the calendar

The calendar has three modes: **予定** (personal due dates), **実績** (actual
run starts and task completions), and **両方**. The choice is saved with the
workspace preferences. Due dates use the local calendar day. An unfinished
task becomes **期限超過** after the end of that day; a task marked complete
or canceled does not become overdue. Editing a due date does not change any
recorded run or completion time.

Select an item to open its task. **+N 件を表示** opens the full list for a
crowded day above the grid; selecting it again closes that list. Planned and
combined modes also show unfinished tasks without due dates under
**期限なし**. Existing source, text, label, and saved-view filters still apply.

## Use a smaller window without losing the current view

Tasks accepts a 720-pixel-wide window. Below 1120 pixels, **ビュー** switches
to the navigation sidebar, and selecting a task opens a full-width detail
panel. **一覧へ戻る (Esc)** returns to the current task view. Resizing keeps
the chosen board, list, timeline, or calendar layout.

Clock-only updates replace age labels in place. Content updates preserve
keyboard focus on the same task or action when it still exists. Open menus
defer structural rebuilding until they close. Scroll positions and editing
forms remain available across routine updates.
