---
name: noa-tasks
description: Read, create, take and report on a repository's tasks for coding agents: local tasks in <repo>/.noa/tasks/*.md and the repository's GitHub issues, listed together, the way the Noa Tasks widget shows them. Use when asked what tasks there are, to add or close a local task, to record a follow-up for later (タスクに積んで, あとでやることを残して), or when started on a task file (.noa/tasks/runs/<run>/task.md) that says to report to $NOA_RESULT. Not for writing to GitHub issues, and not for orchestrating workers (noa-orchestration does that).
---

# Tasks: local tasks and GitHub issues

A repository's tasks come from two sources, shown together by the Noa Tasks
widget (`docs/specs/task-widget.md`) and readable without it:

| Source | Where | You may |
|---|---|---|
| Local task | `<repo>/.noa/tasks/t-<millis>.md`, one file per task | read, create, edit, close |
| GitHub issue | the `origin` repository on GitHub, through `gh` | read only |
| Code marker | a `TODO:` / `FIXME:` / `#TODO(agent):` comment in a tracked file | read only; remove it when its work is done |

`<repo>` is `git rev-parse --show-toplevel` of the checkout you work in. In a
linked worktree that is the worktree, so a task created there stays in that
worktree; create it in the main worktree
(`git worktree list | head -1`) when it should outlive the branch.

`<repo>/.noa/` is gitignored: local tasks are this machine's checkout only and
are never committed. Do not move them into tracked files.

## The local task file

```text
---
title: Reflow loses the cursor row on a narrow resize
state: open
labels: bug, grid
---
Free Markdown body: what is wrong, where to look, what done means.
```

- The header is `key: value` lines between two `---` lines, not YAML: no
  quotes, no lists, no nesting. `title` is required. `state` is `open` or
  `closed` (absent means `open`). `labels` is comma-separated. Keep any other
  key you find as it is.
- Name a new file `t-<milliseconds since the epoch>.md`; if that name exists,
  add `-2`, `-3`, ….
- Write it whole in one step (to a temporary name in the same directory, then
  `ln` it to its name, which fails rather than replace a task already there),
  so the widget never reads half a file and two tasks made in the same second
  both survive. Create the directory and its `.gitignore` holding `*` if they
  are missing:

```sh
repo=$(git rev-parse --show-toplevel)
dir="$repo/.noa/tasks"
mkdir -p "$dir"
[ -f "$repo/.noa/.gitignore" ] || printf '*\n' > "$repo/.noa/.gitignore"
base="t-$(date +%s)000"   # milliseconds; date +%s%3N on GNU date
tmp="$dir/.$base.$$.tmp"
cat > "$tmp" <<'EOF'
---
title: …
labels: …
---
…
EOF
id=$base n=1
until ln "$tmp" "$dir/$id.md" 2>/dev/null; do
  [ -e "$dir/$id.md" ] || [ -L "$dir/$id.md" ] || { echo "cannot create $dir/$id.md" >&2; break; }
  n=$((n + 1)) id="$base-$n"
done
rm -f "$tmp"
```

- To close a local task, change `state: open` to `state: closed` (same
  write-whole-then-`mv`). Close only a task the human asked you to close or one
  your own task file names as done by you; reporting a task finished is not
  closing it (below).

## Listing every task

```sh
repo=$(git rev-parse --show-toplevel)
# Local: id, state, title
for f in "$repo"/.noa/tasks/t-*.md; do
  [ -f "$f" ] || continue
  awk -v id="$(basename "$f" .md)" '
    NR==1 && $0!="---" {exit}
    NR>1 && $0=="---" {printf "%s\t%s\t%s\n", id, (s?s:"open"), t; exit}
    /^title: / {t=substr($0,8)} /^state: / {s=substr($0,8)}' "$f"
done
# GitHub: open issues (read only)
gh issue list --state open --limit 200 --json number,title,labels,url
# Code: markers in tracked files, vendored code left out
git grep -n -I -E '(^|[^A-Za-z0-9_`])#?(TODO|FIXME)(\([^)]*\))?:' -- . \
  ':(exclude,glob)**/vendor/**' ':(exclude,glob)**/third_party/**' \
  ':(exclude,glob)**/third-party/**' ':(exclude,glob)**/node_modules/**'
```

If `gh` says it is not authenticated or the repository is not on GitHub, say
so and go on with the local tasks; do not run `gh auth login` yourself. Read
one issue in full with `gh issue view N --json title,body,comments`.

Present them as one list, local and GitHub mixed, marked `local t-…` or
`#N`. A task the widget has an agent on is visible only in the widget (its
run log is not in the repository), so do not guess which are taken.

## Started on a task by the widget

The widget starts you with a prompt that names
`<workdir>/.noa/tasks/runs/<run-id>/task.md` and with `NOA_RESULT` set.

1. Read the task file. It names the task (a local id or an issue URL), its
   text, and the directory you work in (often a fresh worktree on its own
   branch).
2. Do the task there. Commit on that branch when the task file says so;
   never push, never switch branches, never merge.
3. Write your report to `$NOA_RESULT` before you finish: what you changed (files, commits),
   how you checked it (commands and their outcome), and what is left. For an
   issue, put `Fixes #N` in the commit message only when the work fully fixes
   it. The widget shows the task as 確認待ち once you end with the report
   written, and shows the report beside it.
4. Do not close the task, local or GitHub. Moving it to 完了 is the human's
   decision after reading your report.
5. Something you found that is outside the task: record it as a new local
   task (above) and name it in the report, instead of doing it.

## Never

- Write to GitHub: no `gh issue create/edit/close/comment`, no labels, no
  Projects. The tasks model is read-only on GitHub.
- Edit a local task file another agent may be working from, beyond `state`;
  add a new task instead.
- Delete a task file. A task that should not exist is closed.
