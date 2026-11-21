# `+ctl result PANE --path`

Status: design, not built.

## Goal

`noa +ctl result PANE --path` prints the path of PANE's result file and a
newline, and does not read the file. It is the same path `result PANE` reads:

1. the `result` path this pane's journal recorded for PANE's spawn, or where
   `close --prune` moved it (`journal::workers`, field `Worker::result`);
2. else, for a pane `ls` lists, `<its cwd>/.noa/result-LABEL.md` when its label
   is a plain name (`ctl::result_path` → `result::labelled_path`).

Purpose: an orchestrator can `tail -f "$(noa +ctl result 7 --path)"` on a long
log that a command worker writes to `$NOA_RESULT`. `result --max-bytes` keeps
the start of the file, so it cannot show the end.

## Code today (as read on this branch)

- `crates/noa-app/src/ctl.rs`
  - `USAGE` (const, line 57): the `+ctl --help` text; the `result` entry is at
    lines 170-176.
  - `enum Command`, variant `Result { pane: u64, max_bytes: Option<usize> }`
    (line 262), doc comment `result PANE [--max-bytes N]` (line 261).
  - `parse_args` (line 447), arm `"result"` (line 504): takes `--max-bytes`
    with `Args::value` and `parse_u64`, then the pane with `single_pane`;
    `rest.finish()` refuses anything left.
  - `Args::flag` (line 879) takes a bare flag out by name.
  - `Session::execute` (line 1858) dispatches
    `Command::Result { pane, max_bytes } => self.result(pane, max_bytes)`
    (line 1928).
  - `Session::result` (line 2511) resolves the path (journal first, else
    `self.client.list_panels()` and `result_path`, else
    `Failure::error(no_known_result(pane))`), then calls `result::read` and
    writes the text to stdout.
  - `result_path` (line 3688), `no_known_result` (line 3698).
  - Tests: `status_joins_each_worker_with_its_journal_and_lists_gone_ones_last`
    (line 6273) holds the current `result` parse assertions (lines 6322-6330).
- `crates/noa-app/src/ctl/result.rs`: `labelled_path`, `read` (the only
  function that opens the file), tests module at the bottom.
- An argument error exits 1: `parse_args`'s `Err` is printed with
  `USAGE_HINT` (ctl.rs lines 1150-1156). Existing conflict messages name both
  options, e.g. `"--worktree and --cwd conflict: the worktree is the cwd"`
  (line 638).

## Acceptance criteria and approach

### AC1: `--path` prints the path and a newline, exit 0, whatever the file's state

Approach:

- Add `path: bool` to `Command::Result` and update its doc comment to
  `result PANE [--max-bytes N | --path]`.
- In `Session` (ctl.rs), split the path lookup out of `Session::result` into a
  new method `fn result_file(&mut self, pane: u64) -> Result<PathBuf, Failure>`
  holding today's lookup unchanged (journal → `list_panels` + `result_path` →
  `Failure::error(no_known_result(pane))`). `Session::result` then becomes
  `fn result(&mut self, pane: u64, max_bytes: Option<usize>, path_only: bool)`:
  it calls `result_file`, and with `path_only` writes the path line and returns
  `Ok(EXIT_OK)` before `result::read` is reached; without it, it behaves as
  today.
- The line is built by a new function in `crates/noa-app/src/ctl/result.rs`,
  `pub(crate) fn write_path(out: &mut impl std::io::Write, path: &Path) ->
  std::io::Result<()>`, writing `path.display()` and `\n`. It takes no file
  system call, so an existing, empty or missing file all give the same line.
  `display()` is exact here: both path sources are UTF-8 (`Worker::result` is a
  `String` from the journal; `labelled_path` joins the panel's `cwd: String`
  with an ASCII name).
- `execute` passes the new field: `Command::Result { pane, max_bytes, path }`.

Exit codes: 0 when a path is known; 1 with the unchanged `no_known_result`
message only when no path is known. A failure of `list_panels` (server not
reachable) keeps today's exit code and message; it is not part of AC1.

### AC2: `--path` with `--max-bytes` is an argument error

Approach: in `parse_args`'s `"result"` arm, take `let path =
rest.flag("--path");` next to `--max-bytes`, and when both are given return
`Err("--path and --max-bytes conflict: --path reads no file".to_string())`.
This goes through the existing argument-error path (exit 1, message plus
`USAGE_HINT`). Order of the flags on the command line does not matter, since
`Args` takes options out by name.

### AC3: unit tests

Approach (all in the `impl` unit):

- Parsing, in the ctl.rs tests module, a new test
  `result_parses_path_and_refuses_it_with_max_bytes`:
  - `["result", "4"]` → `Command::Result { pane: 4, max_bytes: None, path: false }`;
  - `["result", "4", "--path"]` and `["result", "--path", "4"]` →
    `path: true, max_bytes: None`;
  - `["result", "4", "--path", "--max-bytes", "10"]` (and the reverse order) →
    `Err` whose text contains both `--path` and `--max-bytes`;
  - `["result", "--path"]` (no pane) → `Err`.
  - Update the existing assertion at ctl.rs line 6324 to carry `path: false`.
- Path-only output, in `result.rs`'s tests module, a new test
  `the_path_is_printed_whether_the_file_exists_is_empty_or_is_missing`: in a
  `temp_dir`, for a file with text, an empty file (`prepare`), and a path never
  created, `write_path` into a `Vec<u8>` gives exactly `"<path>\n"`; afterwards
  the missing file still does not exist and the other two keep their contents
  (the function did not create, read into stdout or change them). This tests
  the function `Session::result` calls with `--path`; `Session` itself needs a
  live server and has no unit test today.
- Check: `cargo test -p noa-app --offline` passes.

### AC4: documentation

- `USAGE` in ctl.rs (unit `impl`): the entry becomes
  `result PANE [--max-bytes N | --path]` and gains a sentence such as
  "--path prints only the file's path (whether it exists or not), e.g. to tail
  a command's log; it does not go with --max-bytes." The column layout of the
  other entries stays as is.
- `skills/noa-orchestration/reference.md`, section `### result PANE
  [--max-bytes N]` (line 285) (unit `docs`): heading to
  `### result PANE [--max-bytes N | --path]`; add a paragraph: `--path` prints
  the path and a newline and exits 0 whether the file exists, is empty or is
  missing; exit 1 only with `pane N has no known result file: …`; `--path` with
  `--max-bytes` is an argument error; example `tail -f "$(noa result PANE
  --path)"` for a command worker's log.
- `docs/AGENT_WORKFLOW.md`, the `+ctl` command table row for `result`
  (line 297) (unit `docs`): `result PANE [--max-bytes N \| --path]`, and
  "`--path` prints only the file's path, e.g. to `tail` a log".

### AC5: `scripts/verify.sh --full` passes

Run after both units are merged. In a sandbox, set `CARGO_NET_OFFLINE=true`;
the `noa-pty` and `noa-ipc` tests need `openpty` and a loopback bind
(AGENTS.md), so run it outside the sandbox. AGENTS.md says clippy
(`-D warnings`) runs only on Linux in CI; this spec does not say whether
`--full` runs it locally on macOS (not checked).

## Risks

- `Command::Result` gains a field: every pattern and literal of it must be
  updated (`execute` at line 1928, the test at line 6324). The compiler finds
  them; there is no other use (grep on this branch).
- Splitting `Session::result` must not change the non-`--path` behaviour:
  keep the lookup order (journal before `list_panels`) and the messages as is.
- `Args::flag("--path")` matches only the bare `--path`; `--path=x` is left in
  the arguments and refused by `single_pane`/`finish` as an unknown option,
  which is the wanted result.
- The printed path can name a file that does not exist yet (a pane with a plain
  label whose worker never got that file, or a reserved name before the first
  write). That is the AC1 contract; the docs must say so, so that an
  orchestrator expects `tail` to wait for or report a missing file.
- `USAGE` is checked by a few tests (e.g. `USAGE_HINT` at line 6607,
  `help_is_only_read_in_the_command_position` at line 7290); a text edit should
  not break them, but `cargo test -p noa-app` is the check.

## Units (disjoint files, buildable in parallel)

### Unit `impl`

Files: `crates/noa-app/src/ctl.rs`, `crates/noa-app/src/ctl/result.rs`.

Work: AC1, AC2, AC3, and the `USAGE` part of AC4 (`Command::Result.path`,
`parse_args` "result" arm, `Session::result_file`, `Session::result`,
`execute`, `result::write_path`, the two new tests and the updated assertion,
the `USAGE` entry).

Done when: `cargo test -p noa-app --offline` passes with the two new tests;
`Noa +ctl --help` shows `--path` in the `result` entry; `git diff --stat`
touches only the two files above.

### Unit `docs`

Files: `skills/noa-orchestration/reference.md`, `docs/AGENT_WORKFLOW.md`.

Work: the reference.md and AGENT_WORKFLOW.md parts of AC4, with the wording of
the behaviour in AC1 and AC2 above (flag name `--path`, conflict with
`--max-bytes`, exit codes, the `tail` use).

Done when: the reference.md `result` section and the AGENT_WORKFLOW.md `+ctl`
table row describe `--path` as in AC4; `git diff --stat` touches only the two
files above.

After both: AC5 (`scripts/verify.sh --full`) on the merged branch.

## Open questions

- None blocks the build. Decided here by default: the conflict message text
  (`--path and --max-bytes conflict: --path reads no file`) and the name
  `write_path`; the `docs` unit should copy the message text from the merged
  `impl` if it differs.
