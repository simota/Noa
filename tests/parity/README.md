# noa-parity — screen-dump parity harness

Fixture-based regression tests for the fidelity core (`noa-vt` + `noa-grid`).
Each fixture feeds a raw byte stream through the real parse→state pipeline
(`noa_vt::Stream` → `noa_grid::Terminal`) and pins the resulting screen as a
plain-text dump. The harness exists to be pointed at a **Ghostty oracle**
later: the same fixture inputs replayed against Ghostty (or xterm) produce
reference dumps, and any line-level divergence is a parity bug by definition.

```bash
cargo test -p noa-parity                      # run all fixtures + unit tests
NOA_PARITY_BLESS=1 cargo test -p noa-parity   # rewrite diverging ## expect: sections
```

## Fixture format

One file per behavior under `fixtures/<name>.txt`:

```
## cols: 10
## rows: 3
## mode: text
## input:
0123456789A
## expect:
0123456789
A

# cursor: 1,1
## why:
Autowrap boundary: the 11th character resolves the pending wrap ...
```

- `## cols:` / `## rows:` — grid size (required, > 0).
- `## mode:` — `text` or `attrs` (see dump formats below).
- `## input:` — the bytes fed to the terminal. Each line is unescaped and
  concatenated; **line-end newlines are never part of the input** (write `\r`
  `\n` explicitly). Escapes: `\e` (ESC), `\r`, `\n`, `\t`, `\\`, `\xNN` (one
  raw byte from two hex digits). Everything else is literal UTF-8, so CJK and
  other multibyte text goes in directly.
- `## expect:` — the expected dump, verbatim. Trailing blank lines are
  ignored (both dump modes end with the `# cursor:` line). Dump lines must
  not start with `## ` — that prefix is reserved for section markers.
- `## why:` — required free text: which xterm/Ghostty behavior the fixture
  pins, and any known noa gap the expectation deliberately records.

## Dump formats

### `text` mode

One line per grid row of the **active** screen (primary or alt), top to
bottom:

- trailing blank cells are trimmed from each line;
- a wide (CJK) cell prints its scalar once — the trailing spacer cell is
  skipped;
- combining marks are emitted attached to their base cell.

The dump ends with a cursor line (0-based row/column):

```
# cursor: <row>,<col>
# cursor: <row>,<col> (pending-wrap)    ← deferred-wrap latch (xenl) set
```

### `attrs` mode

For SGR verification. One line per run of consecutive, identically-styled,
non-default cells (a fully default cell — blank, unstyled — breaks the run
and is never dumped):

```
<row>: [<x0>-<x1>] "<text>" fg=<color> bg=<color> ul=<color> attrs=<flag+flag>
```

- `<row>`, `<x0>-<x1>` — 0-based row and inclusive column range.
- `"<text>"` — the run's visible text (`\` and `"` are backslash-escaped).
  Wide cells fold lead + spacer into one run, so a range wider than the text
  is how the dump encodes wideness (e.g. `[0-1] "漢"`).
- `fg=` / `bg=` — omitted when default; palette colors as the bare index
  (`fg=196`), truecolor as `#rrggbb`.
- `ul=` — underline color, omitted when unset.
- `attrs=` — omitted when empty; `+`-joined flags: `bold faint italic
  underline blink inverse invisible strike overline double-underline
  curly-underline dotted-underline dashed-underline`.

Blank cells carrying only a background (BCE) are dumped — that is the point.
The same `# cursor:` line terminates the dump.

## Bless workflow

1. Add a fixture with headers, `## input:`, an **empty** `## expect:`
   section, and the `## why:` rationale.
2. `NOA_PARITY_BLESS=1 cargo test -p noa-parity` fills in the expect section
   (only that section is rewritten; every other byte is preserved).
3. **Review the blessed dump line by line against xterm/Ghostty semantics**
   before committing — bless records what noa *does*, not what is *correct*.
   If noa is known-divergent, keep the fixture and say so in `## why:` (see
   `scroll_region_origin.txt` for the DECOM gap) so the future fix shows up
   as a deliberate re-bless.

## Future: Ghostty oracle, esctest2, vttest

- **Ghostty oracle** — replay `## input:` into a headless Ghostty (or xterm
  via a pty + screen scrape) and diff its dump against `## expect:` instead
  of trusting the blessed value. The runner is already the right seam:
  `run_fixture_with_mode()` is pure bytes-in/dump-out, so an oracle is just a
  second implementation of the same signature.
- **esctest2** — its per-sequence assertions map onto small fixtures;
  a converter can emit this format directly.
- **vttest** — interactive menus don't convert directly, but captured
  byte transcripts of individual vttest screens can be checked in as
  fixtures once scraped.
