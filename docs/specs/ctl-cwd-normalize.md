# `+ctl spawn --cwd`: print and pass the cwd resolved

Status: design, not built.

## Goal

`noa +ctl spawn --cwd DIR` with a relative `DIR` hands the worker, the journal
and the worker's `NOA_RESULT` a path that still holds `.` and `..`:
`--cwd ../Noa-wt/impl` run from `/x/Noa` gives `/x/Noa/../Noa-wt/impl`. After
this change every place that records the worker's cwd holds one path with no
`.` or `..` components that names the same directory.

Acceptance criteria (from the request):

1. For a relative `--cwd` (including `.` and `..` components), the spawn line's
   `cwd`, the journal's `cwd`, and the `NOA_RESULT` / `result` path contain no
   `.` or `..` components and name the same directory. An absolute `--cwd`
   without `.` or `..` components comes out unchanged.
2. `noa +ctl result` and `close --prune` keep working for such a worker (the
   worktree is found and its report moved).
3. Unit tests cover the cases above; `scripts/verify.sh --full` passes.
4. The `spawn` USAGE text in `crates/noa-app/src/ctl.rs` and
   `skills/noa-orchestration/reference.md` say that `cwd` is printed resolved.

## Code today (as read on this branch, `orch/design` at `cd53915`)

- `crates/noa-app/src/ctl.rs`
  - `spawn_cwd` (line 4074): `(Some(dir), Some(current)) => current.join(dir)`,
    `(Some(dir), None) => PathBuf::from(dir)`, `(None, current) => current?`,
    then `into_string().ok()`. No normalization. `Path::join` keeps `.`/`..`.
  - `check_spawn` (line 1408) calls `spawn_cwd` and refuses a path that is not
    a directory (`is_dir`, which the kernel resolves, so `..` paths pass). It is
    called from `run` (line 1182) before any request.
  - `Session::spawn`: `let mut cwd = spawn_cwd(args.cwd, current_dir)` (line
    2082). That one `cwd` value (replaced by the worktree path under
    `--worktree`, line 2138) goes to
    - `result::prepare(Path::new(dir), label)` → `NOA_RESULT` and `result_path`
      (lines 2140-2157);
    - `started_in = cwd.clone()` (line 2158) → the journal's `"cwd"` (line 2247);
    - `SpawnConfig.cwd` sent to the app (line 2162).
  - The spawn line (lines 2226-2238) prints `paneId`, `recovered`, `worktree`,
    `branch`, `tier`, `result`. **It has no `cwd` field today**; criterion 1
    needs one, so this spec adds it (from `started_in`).
  - The board spawn passes `spawn_cwd(None, current_dir)` (line 2000).
  - USAGE `spawn` entry: lines 63-114 (`prints {"paneId":...}` at line 67,
    `--cwd DIR` at line 72, `NOA_RESULT=<its cwd>/...` at line 109).
  - Test `a_relative_cwd_is_resolved_against_the_orchestrators_cwd` (line
    6400) uses `/repo`, which does not exist.
- `crates/noa-app/src/ctl/result.rs`: `prepare`/`path_for`/`labelled_path`
  build `<cwd>/.noa/result-…md` by `join`, so the result path inherits whatever
  `cwd` holds.
- `crates/noa-app/src/ctl/journal.rs`: records the `"cwd"` it is given
  (`Worker::cwd`, line 206/264). Needs no change.
- `crates/noa-app/src/ctl/worktree.rs`: `prune` (line 362) finds the repository
  from `pane_cwd.ancestors().find(is_dir)`, and `within` (line 499) compares
  `canonicalize()`d paths, falling back to the raw path when one does not
  exist. `name_here` compares canonicalized `.noa/` directories.
- App side: `ipc_spawn_pane` (`crates/noa-app/src/app/ipc.rs:643`) keeps
  `config.cwd` as is (`usable_cwd` only filters out non-directories) and stores
  it in `Orchestration.cwd` (line 757); `card_to_panel`
  (`crates/noa-app/src/ipc_bridge.rs:662`) shows it as `Panel.cwd` until the
  shell reports its own (OSC 7).

Checked in this session (Python, macOS): `chdir("/tmp")` then `getcwd()` gives
`/private/tmp`, and with `link -> real/sub`, `<dir>/link/..` resolves to
`<dir>/real` while lexical normalization gives `<dir>`. Not run here: that
Rust's `std::env::current_dir` returns the same physical path (it wraps
`getcwd` on Unix, per std's source).

## Decisions

### D1. Lexical normalization, checked against the kernel when `..` is present; not plain `canonicalize`

`canonicalize` alone fails criterion 1's second half: it rewrites every
symlinked prefix of an absolute path (`/tmp/x` → `/private/tmp/x`, a symlinked
`~/repos`), so an absolute `--cwd` without dots would not come out unchanged,
and the printed cwd would stop matching what the caller typed.

Lexical normalization alone can name the wrong directory: a `..` after a
symlink goes to the symlink target's parent in the kernel, not to the
symlink's own parent (shown above). Criterion 1 requires the same directory.

The rule, applied by a new private helper in `ctl.rs` to the absolute path
`spawn_cwd` builds (`current.join(dir)`, or an absolute `dir`):

1. No segment of the path (split on `/`) is `.` or `..` → return it
   **byte-for-byte unchanged** (keeps `//`, trailing `/`, symlinks). No
   filesystem access.
2. `.` segments but no `..` → drop the `.` segments (rebuild from
   `Path::components()`, which also drops interior `.`). Dropping `.` never
   changes the directory, so no filesystem access.
3. Any `..` segment → `lexical` = the path rebuilt from components, with
   `ParentDir` popping the last `Normal` component (never past the root:
   `/..` → `/`). Then:
   - `joined.canonicalize()` fails (the directory does not exist) → return
     the joined path unchanged, so `check_spawn` refuses it as today and never
     accepts a different directory that `lexical` happens to name;
   - `lexical.canonicalize()` equals `joined.canonicalize()` → `lexical`;
   - otherwise (a symlink before a `..`) → `joined.canonicalize()`. This is
     the only case where symlinks in the printed path are resolved.

A relative path that stays relative (`(Some(dir), None)`: this process's cwd
is unreadable) is returned unchanged, as today. Non-UTF-8 paths still give
`None`, as today.

### D2. Normalize inside `spawn_cwd`

`spawn_cwd` is the one function that turns `--cwd` into the worker's cwd, and
`Session::spawn` feeds its single return value to `SpawnConfig.cwd`, the
journal, `result::prepare` and (new) the spawn line. Normalizing there means
every consumer, including `check_spawn`'s `(resolved to …)` message and the
app, sees one value; no reader (`result`, `close --prune`, `status`) changes.
`spawn_cwd`'s signature stays the same. It now touches the filesystem only in
rule 3. `check_spawn` and `Session::spawn` each call it, so both see the same
value unless the directory changes in between (already true of the `is_dir`
check today).

### D3. The spawn line gains `"cwd"`

`line["cwd"] = started_in` when known (the worktree path under `--worktree`),
added next to `"result"`. This is an additive JSON field.

### D4. Panes Noa itself reports need nothing

`Panel.cwd` is either the string `+ctl` sent (`Orchestration.cwd`), which is
now normalized by D2, or what the worker's shell reports by OSC 7, which this
change does not touch. No app-side or `noa-ipc` change. Not verified here:
what `$PWD` a given shell reports for a directory reached through a symlink;
that behavior is the same before and after this change.

## Approach per criterion

1. D1 + D2 + D3. The journal `cwd`, the spawn line `cwd`, `SpawnConfig.cwd` and
   `NOA_RESULT` (`<cwd>/.noa/result-…md` from `result::prepare`) all derive from
   the one `cwd` variable in `Session::spawn` (by code reading, lines
   2082-2163), so normalizing `spawn_cwd` covers all of them.
2. `result PANE` reads the journal's `result` path, now normalized, which
   exists. `close --prune` gets the pane's cwd from `Panel.cwd` (normalized
   per D4 until OSC 7) or the journal; `prune`/`within`/`name_here` already
   canonicalize when the paths exist, and with no `..` left the raw-path
   fallback of `within` (used when the pane's cwd was removed) now compares
   like with like. No change to `worktree.rs` or `result.rs` code; a test
   proves it (unit U2).
3. Unit tests in U1 and U2; `scripts/verify.sh --full` after all units.
4. USAGE text (U1) and `reference.md` (U3).

## Files involved

- `crates/noa-app/src/ctl.rs` — helper + `spawn_cwd`, spawn line `cwd`, USAGE,
  tests.
- `crates/noa-app/src/ctl/worktree.rs` — tests only.
- `skills/noa-orchestration/reference.md` — docs.

Not touched: `ctl/journal.rs`, `ctl/result.rs`, the app, `noa-ipc`.

## Units (no two edit the same file)

### U1 — `crates/noa-app/src/ctl.rs`

- Add the D1 helper (private) and call it at the end of `spawn_cwd` on an
  absolute path; update `spawn_cwd`'s doc comment.
- Add `line["cwd"]` from `started_in` to the spawn line (D3).
- USAGE: `prints {"paneId":...}` → mention `"cwd"` (the worker's cwd, printed
  resolved: relative to this process's cwd, `.` and `..` removed); the `--cwd
  DIR` line or its note says the same.
- Tests (temp dirs created under `std::env::temp_dir()` and canonicalized, as
  `worktree.rs`'s `repo()` does):
  - relative `sub/../other`, `./sub`, `.`, `..`, `a/./b` from a real base →
    no `.`/`..` component, and `canonicalize()` equal to the joined path's;
  - absolute without dots, including a trailing `/` and a nonexistent `/repo`
    → byte-for-byte unchanged (the existing test stays);
  - absolute with dots (`<base>/a/../b`) → normalized;
  - symlink before `..` (`link -> real/sub`, `--cwd link/..`) → names
    `<base>/real`, not `<base>`;
  - `..` path whose directory does not exist → returned unchanged, and
    `check_spawn` refuses it;
  - `result::prepare(Path::new(&spawn_cwd(...)), Some("x"))` → a path with no
    `.`/`..` component, inside the normalized dir;
  - `result_path(&workers, &panel)` for a panel whose `cwd` is a normalized dir
    and label `x` → `<dir>/.noa/result-x.md`.
- Done: `cargo test -p noa-app --offline ctl::` passes with the new tests;
  `cargo clippy -p noa-app --offline` has no new warnings.

### U2 — `crates/noa-app/src/ctl/worktree.rs` (tests only)

- Test: with `repo()`, `add(&repo, "job")`, then
  `cwd = crate::ctl::spawn_cwd(Some("../repo-wt/job".into()), Some(repo))`;
  assert it equals `made.path` as a string; `result::prepare(cwd, "job")`, write
  a report; `prune("job", cwd, Some(&own), &[])` → `removed: true`, the report
  moved into `<repo>/.noa/`.
- Same with the pane's cwd a subdirectory given as `../repo-wt/job/sub/.`
  that the worker removed before `prune` (exercises `within`'s raw-path
  fallback).
- Done: both tests pass with U1 merged (the first assertion and the removed
  subdirectory case fail without it); edits only the `#[cfg(test)]` module.

### U3 — `skills/noa-orchestration/reference.md`

- `spawn` (line 65): the printed line is `{"paneId":"…","cwd":"…","result":"…"}`,
  `cwd` the worker's directory printed resolved.
- `--cwd DIR` row (line 86): relative paths are taken from this process's cwd
  and resolved: `.` and `..` removed; an absolute path without them is used as
  given; symlinks are kept except one followed by `..`, which is resolved.
- Done: the two passages say so; no other text changes.

After all three: `scripts/verify.sh --full` passes (`noa-pty`/`noa-ipc` tests
need the sandbox disabled, per `CLAUDE.md`).

## Risks

- Journals written before this change keep `..` paths. Their readers are
  unchanged, so they behave as today, including `within`'s raw-path fallback
  failing for a removed pane cwd with `..`. Not addressed here.
- Rule 3's fallback resolves symlinks in the printed path (e.g. `/tmp` →
  `/private/tmp`) when a symlink precedes `..`. Rare; documented in U3.
- The spawn line gets a new `cwd` field. Readers that parse `paneId`/`result`
  are unaffected; a reader that compares whole lines would see a change.
- `spawn_cwd` now calls `canonicalize` (rule 3 only), twice per spawn
  (`check_spawn`, `Session::spawn`).

## Open questions

- None blocking. The spec adds `"cwd"` to the spawn line, which does not exist
  today, because criterion 1 names it.
