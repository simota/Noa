# Tasks workspace

| | |
|---|---|
| Status | Implemented in the Tasks workspace change; validation belongs to the PR |
| Crate | `widgets/tasks` |
| Model | `src/workspace.rs` |
| Screen | `src/ui/planning.rs`, `src/ui/organizer.rs` |
| Related specifications | [Task sources and agent execution](task-widget.md), [Task links](task-links.md), [Automatic agent assignment](auto-assign.md) |

Tasks adds personal work planning over the existing local tasks, GitHub
issues and code markers. The default list brings navigation, editable
properties, multiple selection, commands and saved views into one window.
Its planning data belongs to Noa on this machine. It does not change the
task sources' formats or require a server-side task model.

This document describes the implemented scope. It supersedes the original
task-widget specification's statements that there is no stored planning
state or due date. That specification still describes the source readers,
agent execution, source-derived columns and run files.

## Three kinds of state

| State | Owner and meaning | What changes it |
|---|---|---|
| Source state | A local task's Markdown `state`, a GitHub issue's open/closed state, or the presence of a code marker | Existing local edit/close/reopen actions; GitHub or source-code changes outside Tasks |
| Work state | The person's Backlog / Todo / In progress / In review / Done / Canceled choice | Noa's property editor, inline menu, bulk operation or command |
| Agent state | Starting, running, waiting for permission/input, error, finished, or pane gone; reports remain separate evidence | Noa's existing run and pane observations |

A task without a manual work state follows the existing source/run
derivation: Todo, In progress, In review or Done. Choosing a manual state
makes it independent of later agent observations. A finished agent does not
automatically mark a manually planned task Done.

A manual state also takes precedence over a source's closed state in the
planning list and filters. A GitHub row shows `GitHub: open` or
`GitHub: closed` separately. Setting an issue to Done in Noa does not
close it on GitHub.

The existing local close/reopen action still writes the task's Markdown
file. When the new source state is observed, the screen clears the manual
override that was present when that action was requested, provided it has
not since changed. A failed source write leaves the manual state intact.
The automatic-state option also clears a manual override without writing
to any source.

## Navigation and layouts

A new workspace opens as **List**. The sidebar offers:

| Destination | Membership |
|---|---|
| All tasks | Every task currently available from the sources |
| Needs attention | The latest run is waiting for permission or input, reports an agent error, or its pane is gone without a result |
| Triage | An open GitHub issue or code marker with no run and no manual work state |
| Active | Effective work state Todo, In progress or In review |
| Backlog | Effective work state Backlog |
| Review | Effective work state In review |
| Done / Canceled | Either finished work state |
| Pinned | Tasks pinned in this workspace |
| Projects and cycles | One destination for each nonempty property value on available tasks |
| Saved views | Named combinations of navigation, filters, layout and order |

Needs attention is a view of current run facts, not a durable notification
inbox. A normally finished run is not an unread notification. There are no
read/unread controls or notification delivery in this change.

List rows show a task's identifier and title, source labels, work state,
priority, project, assignee, latest agent state, dependency count and due
date where present. The state and priority menus edit a row directly;
selection and pin controls are separate from opening its detail. The
property editor is available from a row, the detail pane or the command
palette.

The four layouts have different purposes:

| Layout | Meaning |
|---|---|
| `list` | Personal work planning, including old source-closed tasks while the source still lists them |
| `board` (実行ボード) | The existing four source/run-derived columns: 未着手 / 実行中 / 確認待ち / 完了; source-closed tasks older than seven days are omitted |
| `timeline` | The existing fourteen-day history of agent runs and source close events |
| `calendar` | The existing month view of run starts and source close events |

Board is an execution-status view. A manual work-state edit does not move
a card between its columns. In particular, a source-closed task can have a
manual active work state and still belong to Board's Done column, or be
outside its seven-day window. Timeline and Calendar show events that
happened; due dates and manual Done transitions are not placed on them.

The source, label and text filters combine with the sidebar destination.
Changing to a normal destination clears those filters; opening a saved
view restores its own combination. Navigation counts are destination
totals rather than counts narrowed by the current source or text filter.

## Properties and relationships

| Property | Stored value and behavior |
|---|---|
| Work state | Optional `backlog`, `todo`, `in_progress`, `in_review`, `done`, `canceled`; unset means automatic |
| Priority | `none`, `urgent`, `high`, `normal`, `low` |
| Assignee | A personal free-text value, not an account or permission assignment |
| Project | A free-text grouping value |
| Cycle | A free-text grouping value |
| Due | Empty or a real date in `YYYY-MM-DD` form |
| Estimate | Integer points from 0 through 999; 0 is unset |
| Pin | Brings a task before unpinned tasks in every sort order |
| Parent | At most one task reference |
| Dependencies | Task references that should finish before this task runs |
| Comments | Local text entries with an identifier and timestamp |
| Activity | Property changes and undo records, with timestamps |
| Manual order | A rank used by the manual sort |

Assignee, project and cycle values are trimmed, limited to 500 bytes and
cannot contain control characters. They are not team, project or cycle
entities: there are no memberships, project milestones, capacity,
cycle start/end dates, automatic rollover or burndown calculations.

Parent and dependency choices use the available tasks and preserve
existing references whose source task is no longer listed. A task cannot
reference itself as a parent or dependency. Parent cycles and dependency
cycles are rejected separately before any part of a transaction is saved.
Parent links do not create a nested task tree, and completing a parent does
not complete its children.

The widget refuses a new run when the task has a manual Done/Canceled
state, or a dependency's effective work state is not Done/Canceled. A
dependency outside the available task list also blocks the run. These
checks apply when opening the run form and again when submitting it.
Existing checks for a malformed task or an already active run remain in
place. Dependencies do not schedule work or launch agents automatically.

Comments are local to this workspace; they are not posted to GitHub or to
an agent. The property editor displays the latest twenty comments and
activity entries and indicates when more exist. Comments are retained
subject to the file size limit; activity retains the latest 200 entries
per task. The editor submits only the fields the person changed, plus a
new comment when provided.

## Commands, selection and order

The command palette searches operations, navigation destinations and
available task titles. It exposes task creation, search, properties, work
states, priorities, pinning, agent execution, selection, undo, saved views,
sorting, layouts, refresh and sidebar visibility.

| Key | Operation |
|---|---|
| `⌘/Ctrl K` | Open the command palette |
| `C` or `N` | Create a local task |
| `J` / `K`, up / down | Move the current row or card selection |
| `H` / `L`, left / right | Move between Board columns |
| `Enter` | Open the selected task's detail |
| `S` | Open work-state commands for the current selection |
| `P` | Open priority commands for the current selection |
| `E` | Open the selected task's property editor |
| `F` | Toggle the selected task's pin |
| `Space` | Toggle the selected task in the multiple selection |
| `⌘/Ctrl A` | Select the tasks drawn in the current layout |
| `⌘/Ctrl Z` | Undo the last workspace property operation |
| `⌘/Ctrl ↑` / `⌘/Ctrl ↓` | In List, move the selected task one position and use manual order |
| `⌘/Ctrl B` | Show or hide the sidebar |
| `/` | Focus search |
| `V` | Cycle layouts |
| `[` / `]` | Move Calendar one month |
| `X` | Open the agent run form |
| `D` | Close or reopen a local source task |
| `A` | Add a related result link using the existing form |
| `O` | Open a selected GitHub issue in the browser |
| `R` | Refresh the source data |
| `Esc` | Close a form, leave search, or close detail |

The palette also offers shortcut/search help. Ordinary task shortcuts do
not run while a text input or form is active; a palette/menu owns its keys
while open. Command shortcuts that would change the task selection or
undo a property edit also leave text editing alone.

State and priority commands apply to the multiple selection when it is
nonempty, otherwise to the current task. The bulk toolbar gives the same
two edits. A bulk change is one validated, atomic transaction and one undo
entry. Selection is limited to tasks drawn by the current layout, including
when a date layout omits otherwise matching tasks.

Sort orders are latest activity, priority, title and manual order.
Latest activity includes the source update, the latest run start and the
latest local property activity. Pins always lead. Manual moves do not
cross the pinned/unpinned boundary, and filtered-out tasks keep their
existing rank. Manual moves are available in List only. There is no
drag-and-drop reordering or column move.

Undo is per window, with up to fifty operations. It restores the tasks
affected by that operation while preserving changes to other tasks and
retaining an audit trail. If an affected task has since been changed
elsewhere, undo refuses to overwrite it. Undo does not reverse source
file edits, GitHub actions, agent execution, saved-view edits or preference
changes. It does not survive restarting the window and has no redo.

## Search

Search is case-insensitive. Whitespace-separated terms are ANDed; double
quotes preserve a phrase, and `-` negates one term. Unqualified text
searches task titles, identifiers, available bodies, task labels, local
comments, assignee, project, cycle, due date, effective work state, latest
run report and agent-state text.

GitHub bodies and comments are included only after their detail has been
loaded. Typing a search does not fetch every issue's detail.

| Qualifier | Matches |
|---|---|
| `state:`, `status:` | Effective work-state word or displayed label |
| `priority:` | Priority word or displayed label |
| `label:` | A source label |
| `agent:` | Latest run's agent command, such as `codex` |
| `project:`, `cycle:`, `assignee:` | The corresponding local property |
| `source:` | `local`, `github`, `code`, or the source's searchable label |
| `due:` | Text within the stored date; no date arithmetic |
| `pinned:` | Whether the task is pinned |
| `blocked:` | Whether any dependency reference exists |
| `attention:` | The same run facts used by Needs attention |
| `is:` | `pinned`, `blocked`, `attention`, `active`, or `done` |

Boolean qualifiers accept `true/false`, `yes/no` and `1/0`.
`is:done` includes Done and Canceled. `blocked:true` and
`is:blocked` mean a dependency relation exists, even when every dependency
has finished. They do not mean that execution is currently blocked. The
run check resolves the dependencies' actual states separately.

String qualifiers use substring matching. State words use underscores,
for example `state:in_review`. An unknown qualifier is treated as literal
search text; it is not silently discarded. There is no OR, grouping,
comparison operator or remote search.

Examples:

```text
state:in_review priority:high -source:code
project:"Tasks 改善" assignee:自分 -is:done
agent:codex attention:true
due:2026-12 pinned:true
```

## Saved views and startup

A saved view stores its name, underlying navigation destination, query,
source filter, label filter, layout and sort order. It appears in the
sidebar and can be deleted through the palette. Saving from a saved view
uses its underlying destination, avoiding recursive view references.
Editing an existing view's settings means saving a new view; there is no
shared view or view editor.

Preferences keep the last destination, query, source and label filters,
layout and sort order. Startup restores them. `--layout=` overrides the
saved layout, and a workspace without preferences uses List.
`--select=` retains its existing role of locating a task and opening
detail, clearing filters so the requested task can be located.

```sh
tasks --repo=/path/to/repo
tasks --repo=/path/to/repo --layout=board
tasks --repo=/path/to/repo --select='#12'
tasks --demo --layout=list --snapshot=/tmp/tasks.png
```

The existing `--demo`, `--snapshot` and `--size` modes remain
available. Demo and recording/test feeds keep workspace changes in memory.

## Persistence and competing windows

The live feed places this state beside the existing run log and GitHub
listing cache:

```text
<data dir>/noa/tasks/<repo-key>/workspace.json
```

The repository key is the existing path-based `store::repo_key`; this is
local to that repository path on this machine. It does not sync through
Git, GitHub or Linear. Local Markdown tasks, run logs and task-result links
keep their existing formats.

The versioned JSON document contains task metadata, preferences and saved
views. Task keys reuse the serialized `TaskRef` representation: source
identity, GitHub repository and number, and a code marker's path and digest
remain distinct. A missing file or metadata entry supplies defaults; no
migration writes into an existing task source.

Before a write, the store validates all proposed changes and relation
graphs. It takes a nonblocking advisory file lock, compares the on-disk
bytes with the last version it read, and replaces the file atomically
through `fsx`. Revision numbers advance on successful commits. Comparing
bytes also detects a hand edit that did not change the revision. The
repository directory and data/lock files are opened without following
symlinks beneath the configured trusted Tasks root.

A conflicting write reports an error instead of replacing another
window's updates. The screen wakes every two seconds and reloads external
state on wake and through the palette's refresh command. Property forms
keep their draft across reloads; a field changed in another window while
being edited is detected
before applying the form. Changes to unrelated properties can coexist,
and a new comment appends to the current conversation.

Invalid JSON, unsupported versions, invalid data, and files over 16 MiB
are refused without overwriting the original. An initial load failure
shows a notice and opens an in-memory workspace whose edits are not saved;
after recovering the file, reopen the window to resume persistence.
A later read or write failure retains the last good in-memory state and
reports the failure. Preferences and saved-view writes use the same
conflict checks as task changes.

GitHub remains read-only. A refreshed listing whose issue `updatedAt`
changed invalidates and refetches detail that was previously requested,
so an open body/comments view can catch up without being reopened.
Unchanged listings keep their cached details, and a failed listing leaves
the existing data available. A changed detail that fails to reload reports
its failure and can be retried.

## Validation responsibilities

| Area | Code / tests |
|---|---|
| Defaults, independent work state, task identity and property search | `src/workspace.rs` inline tests |
| Atomic bulk changes, undo, dates, cycles, persistence, conflicts and damaged files | `src/workspace.rs` inline tests |
| Property deltas, agent labels and draft conflict checks | `src/ui/organizer.rs` inline tests |
| Inline row controls, navigation, keyboard commands, selection and saved views | `src/ui/planning_tests.rs` |
| Run guards and command dispatch | `src/ui/planning.rs`, `src/ui/mod.rs`; native interaction still needs verification |
| Existing source/agent behavior and dated layouts | `src/ui/tests.rs`, `src/live.rs` tests |
| Changed GitHub detail invalidation and retry | `src/github/tests.rs` |

Run `cargo test --manifest-path widgets/tasks/Cargo.toml --locked` for
the Tasks crate, then the repository's normal verification gates. Native
window layout, mouse/keyboard behavior, scroll position, input methods and
narrow-window readability still require manual visual verification.

