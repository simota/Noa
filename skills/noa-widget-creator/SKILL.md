---
name: noa-widget-creator
description: >-
  Create or edit declarative Noa repository widgets from a user goal: generate
  widget.json, bounded data schemas, fixtures and action documentation; validate
  them and inspect offline previews before sharing. Use for requests to create
  a widget, make an agent-generated repository dashboard, turn results into a
  reusable view, or promote a one-off definition (ウィジェットを作る、リポジトリで共有する).
  Use noa-widgets for operating existing live instances. This skill creates
  repository files, not native Rust widgets or personal skill installations.
---

# Create a repository widget

Turn the requested view into one `noa.widget/v1` manifest and example data.
Keep collection and side effects in the agent's existing tools. The manifest
contains presentation, schemas and `agent-event` declarations only.

Read [manifest.md](references/manifest.md) before changing the schema or view.
Use [the status template](assets/status/widget.json) and its
[fixtures](assets/status/fixtures/normal.json) as a small valid starting point.
Resolve bundled resources relative to this SKILL.md, including when this
skill was installed as a symlink outside the target repository.

## 1. Choose the contract

1. Read the target repository's instructions and existing `.noa-widgets`
   packages. Resolve its current Git worktree root.
2. Identify the view's purpose, complete input data shape, update moment and
   permitted actions. Use the user's task to make ordinary choices; ask only
   when a missing answer materially changes the result.
3. Prefer the smallest existing component set that explains the data. Use
   stable IDs for table/list records. Bound strings and arrays in dataSchema.
4. Define each button as a named intent the agent can handle. Document its
   payload and meaning. Do not place a shell command, script, URL fetch or
   executable entry point in the manifest.
5. Choose a unique lowercase-hyphen package ID and stable `major.minor.patch`
   version. Match the directory name to the manifest ID.

## 2. Create the package or draft

For a shared widget, create these tracked files under the target worktree:

```text
.noa-widgets/<id>/widget.json
.noa-widgets/<id>/README.md
.noa-widgets/<id>/fixtures/normal.json
.noa-widgets/<id>/fixtures/empty.json
.noa-widgets/<id>/fixtures/error.json
.noa-widgets/<id>/fixtures/long.json
```

Use `.noa-widgets/` for shared definitions. `.noa/` holds ignored local task
and run state; do not put a shareable definition there. Do not edit another
package's files or ignore rules to make a new package appear shared.

For a one-off view, keep the same definition and fixtures in a local draft
directory outside the tracked package registry. Later, `open
--definition-file=-` can consume the definition on stdin without writing a
repository package. Promotion means explicitly creating the package files;
opening an inline instance does not promote it.

Copy the template if useful, then replace its ID, title, description, data
schema, initial data, view and action names to match the task. Keep README
specific: explain field meanings, the producer's data source, how actions are
handled, and which observations the widget cannot establish by itself.

## 3. Validate before opening

Resolve the Noa executable: `$NOA_BIN` in a Noa pane, the user's installed
`Noa`, or their selected build. These commands do not need a control server:

```sh
widget_noa=${NOA_BIN:-Noa}
"$widget_noa" +ctl widget schema
"$widget_noa" +ctl widget validate "$package/widget.json"
for state in normal empty error long; do
  "$widget_noa" +ctl widget validate "$package/widget.json" \
    --data-file "$package/fixtures/$state.json" || exit 1
done
```

Set `package` to the actual package or draft directory. Use `widget validate`
as the authoritative check: editor JSON Schema alone cannot establish pointer
types, unique node/row IDs, action references or rendered-content limits.

Keep every fixture valid. `error.json` represents a failed operation or
unavailable result within the declared data contract; it is not malformed
JSON. `empty.json` exercises zero results, `normal.json` a useful populated
view, and `long.json` long text, CJK and multiple rows within the bounds.
Check initialData separately as the initial/loading or empty state.

If the binary lacks widget commands, identify that limitation and provide
the files plus checks still needed. Do not describe unrun commands as passed.

## 4. Inspect previews

Create a local output directory for PNGs. Keep rendered PNGs and live data out
of the shared package unless the task explicitly needs them committed.

```sh
preview_dir=$(mktemp -d)
for state in normal empty error long; do
  "$widget_noa" +ctl widget preview "$package/widget.json" \
    --data-file "$package/fixtures/$state.json" \
    --snapshot "$preview_dir/$state.png" --size 960x720 || exit 1
done
"$widget_noa" +ctl widget preview "$package/widget.json" \
  --data-file "$package/fixtures/long.json" \
  --snapshot "$preview_dir/narrow.png" --size 480x640
```

Inspect the images for clipping, overlap, misleading counts, absent labels,
and unreadable long data. Check an interactive preview when keyboard,
scrolling or click behavior matters. Preview buttons display the action
locally and do not perform a live side effect. A successful PNG command is
not proof that the view was visually reviewed.

Revise the contract and all affected fixtures together, then repeat only the
validation or preview needed to verify the change. If fonts or the helper are
unavailable, record the specific unverified part.

## 5. Make it usable and shareable

For a live demonstration, use `noa-widgets` to list, open, publish data and
handle events. Reuse the exact validated definition. Existing instances keep
their pinned definition, so close and open anew to demonstrate a manifest edit.

Inspect `git status` and the package diff. Confirm it includes only the
definition, documentation and synthetic fixtures, with no credentials,
absolute personal paths, runtime state or preview files. Commit the package
when the task or repository workflow calls for a commit; publishing or pushing
follows that task's authorization. Do not install this skill into a person's
agent configuration as part of creating their widget.

Report the package ID and files, the data/action contract, validation results,
the previews actually inspected, and any remaining limits. V1 does not add
custom components, command collectors, network access, embedded HTML/JS,
automatic watchers, hot reload or automatic agent resumption.
