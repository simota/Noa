# Manifest v1 authoring reference

## Contents

- [Package and top-level contract](#package-and-top-level-contract)
- [Schema subset](#schema-subset)
- [Bindings and components](#bindings-and-components)
- [Actions and runtime](#actions-and-runtime)
- [Limits and review](#limits-and-review)

## Package and top-level contract

Use `.noa-widgets/<id>/widget.json` in the current worktree. Keep definitions
tracked and fixtures synthetic; store local instance state separately. The
runtime reads only this worktree's registry, without following symlinks, and
never falls back to another worktree's definition.

Required fields are `apiVersion`, `id`, `version`, `title`, `description`,
`dataSchema`, `initialData`, and `view`. Include `actions` explicitly, using
`[]` for a display-only widget. `apiVersion` is exactly `noa.widget/v1`.
`version` is a stable numeric `major.minor.patch`; prerelease/build suffixes
are not supported. Do not add a top-level `$schema`, permissions, collectors,
scripts or dependencies: unknown fields are rejected.

Package, node and action IDs start with a lowercase ASCII letter, use lowercase
letters/digits/single hyphens, and contain at most 64 characters. Node IDs are
unique throughout the view; action IDs are unique among actions. The package
directory name equals the manifest ID. Titles and descriptions are nonempty,
with at most 200 and 2,000 characters respectively.

## Schema subset

Both `dataSchema` and every `payloadSchema` have an object at the root. Every
schema node must declare one type; use only these keywords:

| Type | Additional supported keywords |
| --- | --- |
| Any supported type | `enum` |
| `object` | `properties`, `required`, boolean `additionalProperties` |
| `array` | Required `items`, optional `minItems`, `maxItems` |
| `string` | `minLength`, `maxLength` |
| `number`, `integer` | `minimum`, `maximum` |
| `boolean`, `null` | No extra type-specific keywords |

Prefer `additionalProperties: false` and make all displayed fields required.
Give array and string properties sensible bounds. Choose non-null empty
values for required fields instead of a type union. There is no `$ref`,
`oneOf`, `anyOf`, `pattern`, `format`, `default`, remote schema loading or
arbitrary validation code. Do not add JSON Schema annotations that are not
in the supported list.

Validate both initialData and every produced update. Data updates replace the
complete object; they are not patches. Empty and failed states use the same
schema, for example a status message plus an empty rows array.

## Bindings and components

Use a literal or `{"source":"data","pointer":"/field"}` for bindable fields.
Pointers use RFC 6901: `/a~1b` selects the property `a/b`, `/a~0b` selects
`a~b`, and an empty pointer selects the root. Escape `~` as `~0` and `/` as
`~1`; do not use `#/field` or `${...}` expressions. A binding must point to a
declared schema location and resolve in each actual data object with the
component's expected type. There is no expression or template evaluation.

Every component has `type` and `id`.

| Component | Required fields | Optional fields |
| --- | --- | --- |
| `column`, `row` | `children` array | `gap` |
| `section` | Bindable string `title`, `children` | `gap` |
| `text`, `badge` | Bindable string `text` | `tone` |
| `metric` | Bindable string `label`, bindable string/number `value` | None |
| `progress` | Bindable numeric `value` | Bindable numeric `max` (default 1), bindable string `label` |
| `table` | Array/binding `rows`, `rowKey`, nonempty `columns` | None |
| `list` | Array/binding `items`, `itemKey`, `textPointer` | None |
| `button` | Bindable string `label`, declared `action` ID | Object/binding `payload` (default `{}`), `inputs`, boolean `clearInputs` |
| `text-input` | None | Bindable string `label`, literal `placeholder`, boolean `multiline`, positive `maxLength`, `submit` button ID |

`gap` is `none`, `xs`, `sm`, `md`, or `lg`. `tone` is `default`, `muted`,
`success`, `warning`, or `error`. Progress requires `0 <= value <= max` and
`max > 0` for every data state.

Table/list arrays hold objects. `rowKey` / `itemKey` are pointers relative to
each row, normally `/id`, resolving to a unique nonempty string or 64-bit
integer. Keep keys stable across updates. `columns` contains objects with
exactly `title` (a nonempty literal string) and `pointer` (row-relative). Cell
values are scalars; do not bind a table cell to an object or nested array.
`textPointer` is row-relative and must resolve to a string.

```json
{
  "type": "table",
  "id": "results",
  "rows": {"source": "data", "pointer": "/results"},
  "rowKey": "/id",
  "columns": [{"title": "Result", "pointer": "/label"}]
}
```

## Actions and runtime

Declare `{"id":"refresh","kind":"agent-event","payloadSchema":{...}}`
in `actions`, then reference `"action":"refresh"` from a button. Define an
object payload schema. For a no-argument action use:

```json
{"type":"object","properties":{},"additionalProperties":false}
```

Use `"payload":{"source":"data","pointer":"/request"}` to bind the
whole payload object. Nested interpolation inside a literal object is not
implemented. An object containing `source` or `pointer` is treated as a
binding, so put business payloads with those keys in data and bind them as
one object.

To let a person type the request, add `text-input` nodes and list them in
the button's `inputs` (payload property → text-input node ID). The host puts
each input's current text into the payload as a string. Declare each such
property in `payloadSchema` with `"type":"string"` and a `maxLength`, keep it
out of the button's own `payload`, and keep the input's `maxLength` at most
the schema's. Set the input's `submit` to that button so Enter (Cmd+Enter in
a `multiline` input) sends it; Enter during an IME conversion never sends.
`clearInputs: true` clears the inputs after the event is accepted. Typed text
is not widget data: it is not stored in instance state and survives data
updates only on screen. Tell the handling agent, in the README, that the
property is free text to read, never a command or path to run.

```json
{"type":"text-input","id":"note","label":"Note","maxLength":500,"submit":"send"},
{"type":"button","id":"send","label":"Send","action":"note",
 "inputs":{"note":"note"},"clearInputs":true}
```

Document how the agent handles each action in the package README. The host
only queues an event; the agent performs authorized work through its normal
tools and publishes new data. The host never executes a command or fetches
data from a repository path or URL. Buttons in offline preview only show
the event locally. Live processing uses a single consumer, eventId
deduplication and acknowledgment after work is durably recorded; load
`noa-widgets` when demonstrating it.

## Limits and review

| Resource | Limit |
| --- | ---: |
| Manifest / data / action payload | 64 / 128 / 16 KiB |
| JSON depth / JSON nodes | 32 / 20,000 |
| One JSON string | 64 KiB |
| View nodes / actions | 512 / 64 |
| Text inputs per button / text-input `maxLength` / placeholder | 16 / 16,384 / 200 characters |
| Table columns / table or list rows | 32 / 1,000 |
| Expanded nodes, rows and cells across the view | 8,192 |
| Displayed text across the view | 512 KiB |

Repeated bindings count toward expanded-content limits. A small data array
referenced by many tables can still exceed the budget. `widget schema`
supports editor tooling; `widget validate` remains the authoritative check,
including schema/view compatibility and actual row keys. A future API version
or unsupported field fails validation rather than being silently ignored.

Inspect normal, empty, failed and long-data fixtures at normal and narrow
sizes. Do not infer that an empty list proves success: express collection
failures or missing results in a visible status message. Include only facts
the producer has established. Snapshot rendering verifies a concrete layout;
live interaction still needs its own observed check.
