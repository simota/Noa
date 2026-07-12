# Spec: Loading Ghostty Config Files (ghostty-config)

## Metadata

- slug: `ghostty-config`
- title: Loading Ghostty Config Files (Ghostty Config File)
- status: **locked** (signed off 2026-07-03)
- owner: simota
- build-path: **orbit loop (engine: codex)**
- recipe: /nexus spec — FRAME ✓ (problem statement approved 2026-07-02: target=both / TOML=replace / key scope=syntax foundation + keys equivalent to existing) / EXPAND ✓ (direction B adopted alone + implement through to semantics) / CHALLENGE ✓ (10 consensus items + ⚠A-D) / SHAPE ✓ (Spark) / SPECIFY ✓ (Accord, re-verified against actual code) / Quality Gate ✓ (Run 1 FAIL → revised → Run 2 PASS) / LOCK ✓ (2026-07-03: ⚠A-E finalized + signed off + build-path chosen)

## L0 — Vision

1. **Target audience**: dotfiles-driven users who are migrating from Ghostty, plus users of noa's own configuration.
2. **Job**: noa should read config files written in Ghostty-native syntax (line-oriented `key = value`) and use them as startup configuration, so that Ghostty config assets (dotfiles) can be reused as-is.
3. **Definition of success**: a noa-native config (in Ghostty syntax) can be read + if no noa config exists, noa reads from Ghostty's config path ("both" ruling). Unknown keys warn + are ignored, just as in Ghostty.
4. **Scope boundary**: v1 is syntax foundation + keys equivalent to what already exists = 4 scalar keys (window-width/window-height/font-size + theme [⚠E adopted 2026-07-03: theme-selection reached completion and was promoted to a shipped key]). Extending to list-type keys (keybind/font-family/palette, ...) is a later increment / a separate spec.
5. **Constraints**: the existing TOML config is **retired and replaced** (now, before there's any backward-compatibility debt, is the right time to migrate). The `noa-config` machinery (path discovery, precedence default < file < CLI, validation) is reused; only the parser is swapped out.

### FRAME ruling (2026-07-02, confirmed by user)

| Point | Ruling |
|------|------|
| Interpretation of "read Ghostty's config file" | **Both** — a noa-native config (Ghostty syntax) is primary, with a fallback to Ghostty's config path when absent (or import — the mechanism is decided during EXPAND/CHALLENGE) |
| Existing TOML config | **Replace** (retire TOML, unify on one parser; the theme-selection draft, which assumed the new format, needs revision) |
| v1 key scope | **Syntax foundation + keys equivalent to what already exists**. Unknown keys warn + ignore (Ghostty behavior). Key expansion is a separate increment |

### Reuse / constraint findings (Lens reuse-scan, 2026-07-02)

- **`noa-config` crate already exists** (deps: anyhow/dirs/toml_edit, GUI-free). Path discovery `default_config_path()` (lib.rs:77-79 → on macOS, `~/Library/Application Support/noa/config.toml`), the precedence model `load_file_overrides()?.merge(cli).apply_to(default)` (lib.rs:59-65), and validation can be reused. **The TOML parser portion (lib.rs:92-170) is incompatible with Ghostty syntax → must be replaced**.
- **The config flow is a single path**: `bin/noa/src/main.rs:21-30` (clap → load_startup_config → AppConfig) → `noa-app/src/app.rs` (cols/rows → app.rs:214/327-332, font_size → app.rs:222/246/605). New keys pass through this chain by adding a field.
- **`noa-app` does not depend on `noa-config`** (the binary is the sole bridge) — this boundary is maintained.
- **Handling of unknown keys is the exact opposite**: the current `reject_unknown_keys` hard-fails (lib.rs:120-131), whereas Ghostty warns and ignores. The test `unknown_key_is_rejected` (lib.rs:291-296) will inevitably need rewriting (the same point raised in the theme-selection draft).
- **A keybind trigger parser is already implemented** (noa-app/src/commands.rs:299-325, `cmd+shift+f` format) — a future landing spot for a `keybind =` key.
- **Adding a String-valued key loses the `Copy` derive** (StartupConfig/ConfigOverrides) — the same ripple effect as theme-selection draft BLOCKER-1. It's reasonable to handle this together when the parser is replaced.
- **Related plans**: parity-plan Phase 3 "Config system expansion" (Ghostty key set + live reload Cmd+Shift+,), Phase 4 "config file." README inc-3/4. **theme-selection.draft.md (at the SPECIFY stage) assumes TOML — the ruling in this spec requires revising its R-1/AC-1–3** (recorded as an interaction).
- **Live reload is not wired up** (every consumer reads once at startup). A grid resize path (io_thread.rs:125) exists but is for window resize. The v1 scope decision is left to CHALLENGE.

## EXPAND — Candidate directions (Riff ‖ Flux ‖ FACT-checking, 2026-07-02)

### FACT-check results (actual Ghostty 1.3.1 behavior, sources: ghostty.org docs / ghostty-org/ghostty)

- **VERIFIED — path resolution is a "read-all-and-merge across 4 candidates"**: ①`$XDG_CONFIG_HOME/ghostty/config.ghostty` ②same dir `config` ③`~/Library/Application Support/com.mitchellh.ghostty/config.ghostty` ④same dir `config`, read in this order and **merged with later entries overriding earlier ones** (not first-wins). If none exist, there's no error — the built-in defaults are used. The `.ghostty` extension was introduced in 1.2.3+. noa's existing `find_first_existing_config_path` does a first-wins single selection, which is **semantically different**.
- **VERIFIED — syntax**: `key = value`, whitespace around `=` is ignored. `#` comments are **valid only at the start of a line** (no trailing comments). Quoting is optional (required only for things like a literal specified via a `?` prefix). **An empty value `key =` resets to the default** (for list-type keys it clears the accumulated list). Keys are lowercase kebab-case and are case-sensitive.
- **PARTIAL — duplicate keys**: scalars are last-wins; list-type keys such as `keybind`/`palette`/`font-family`/`config-file` accumulate (append). There is no explicit official enumeration (confirmed indirectly from individual reference pages + the man page).
- **VERIFIED — `config-file` include**: list-type; relative paths are resolved against the referencing file; the `?` prefix makes it optional; **a cycle is an error ("cycle detected")**; **processing happens at the end of the file** (deferred evaluation — later keys can't override the included file).
- **PARTIAL — error handling**: unknown keys ("unknown field") and invalid values ("invalid value") accumulate as Diagnostics and **parsing continues** (the only fatal case is OOM). At startup, these are shown via a "Configuration Errors" GUI dialog + stderr log. It does not hard fail.
- **VERIFIED — `window-width`/`window-height`**: a grid cell count (integer), affecting **only the initial size of a new window**. **Ineffective unless both are set**. Clamped to a minimum of 10×4. **The keys `cols`/`rows` do not exist in Ghostty**. The priority relationship with `window-save-state` is PARTIAL (presumed that the restored value takes priority on restore).
- **VERIFIED — `font-size`**: accepts float (points), rounded to the nearest integer pixel.
- **VERIFIED — CLI support**: **every config key is also a CLI flag** (`--font-size=14`). CLI > file. `--config-file` reads an additional file (doesn't stop default discovery); `--config-default-files=false` skips default discovery.
- **PARTIAL — reload**: `reload_config` defaults to macOS `cmd+shift+,` + `SIGUSR2`. Whether live application applies is per-key (no uniform rule).

### Candidate directions (Riff)

**A. Fast Path — minimal parser swapped in-place + live fallback** — replace only the parser portion of `noa-config` with a minimal line-based implementation (keeping discovery/precedence/validation as-is). If there's no noa config, read the Ghostty path on the spot. Repeated keys, include, and empty-value reset are unimplemented in v1. Smallest diff, but real dotfiles become "readable but semantically incomplete," carrying a rework risk to the syntax machinery in later increments.

**B. Faithful Import — deep syntax parser + one-shot import** — implement the core semantics (repeated-key accumulation, empty-value reset, include recursion/cycle detection) from the start. Ghostty config is never read live; instead, an explicit import writes supported keys out to the noa config (unsupported keys are visualized as comments). Zero syntax rework and settings converge on a single source of truth. Heavy up-front investment for v1's 3 keys, and without running the import, the "both" experience is weak.

**C. Include-Based Bridge — integrate via the `config-file` directive** — instead of a dedicated fallback path, implement the include mechanism so that when there's no noa config, a virtual `config-file = ~/.config/ghostty/config` (optional) is implicitly prepended. Users can also mix in Ghostty assets deliberately. However, "implicit include" is a noa-specific composition behavior that doesn't exist in Ghostty, and needs justification against the faithful-clone philosophy.

**D. Clean Room Parser — pure-function parser separation + faithful path resolution** (orthogonal to A–C/E as an implementation choice) — separate the parser as an I/O-less pure function `&str -> Vec<Directive>` consumed by `noa-config`. Unit tests can be written without file I/O. Also faithfully reproduces Ghostty's read-all-4-candidates-and-merge path resolution rule.

**E. Layered Merge — always merge instead of falling back** — add one more layer to the existing `ConfigOverrides::merge`, always composing layers as `ghostty_file.merge(noa_file).merge(cli)`. On the noa side, only the diff needs to be written. However this deviates from the FRAME ruling of "fall back only when absent," worsens the debugging experience of a two-file precedence, and is a concept that doesn't exist in Ghostty.

### Flux's premise challenges (across directions)

1. [FACT→VERIFIED] "The Ghostty path" isn't singular — it's a read-all-4-candidates, later-wins merge. The set of fallback target paths and the merge order must be made explicit in the spec.
2. [FACT→VERIFIED] `cols`/`rows` ≒ `window-width`/`window-height` are only superficially equivalent — on the Ghostty side, "both required, coupled with window-save-state, clamped to 10×4." noa has no window-state restoration → need to decide whether this must be documented as a fidelity gap.
3. [DESIGN] "warn+ignore" isn't monolithic — there are at least 3 tiers: unknown-key warn / type-mismatch warn / theme-file-internal forbidden-key silent. For v1, without deciding which error classes get non-fatal treatment, fidelity ends up in the awkward middle ground of "unknown key warns, but type mismatch aborts startup."
4. [DESIGN] If a raw Ghostty config is read live, dozens of noa-unsupported keys would produce a **warning flood on every startup**. If live fallback is adopted, a policy for suppressing warnings from unsupported Ghostty-file-originated keys is needed.
5. [DESIGN] "Reading another app's config" isn't observable Ghostty behavior — it's a **noa-specific convenience extension**. Following the precedent of the theme spec's `light:dark` syntax, decide where to note "this is a noa extension, not fidelity."
6. [DESIGN] The TOML-retirement migration UX is undefined — if the existing config.toml is silently ignored, it will provoke regression reports like "my startup size changed on its own." An explicit decision is needed: detect + warn / auto-convert / do nothing.
7. [FACT→VERIFIED] Ghostty accepts every config key as a CLI flag. noa's existing `--cols`/`--rows` have no counterpart in Ghostty → without deciding rename vs. keep noa's own names vs. coexistence, the same concept ends up with different names between config and CLI.
8. [FACT→VERIFIED] `config-file` has the non-intuitive rule of being "processed at end of file." Even if unimplemented in v1, the behavior on encountering it (ignore/warn/error) must be decided now — otherwise enabling it in a later increment causes a silent behavior change.
9. [DESIGN] There are two semantics: scalar (last-wins) / list (append + empty-value reset). All 3 keys in v1 are scalar, but if we claim to have a "syntax foundation," the parser's behavior when it encounters list syntax or an empty-value reset needs to be decided now, or the parser will need redesigning in the next increment.

### EXPAND checkpoint results (2026-07-02, user ruling)

- **Direction B (Faithful Import) adopted alone** — A/C/E are rejected without going to CHALLENGE (moved to Considered but rejected). D (pure-function parser separation) is treated as an orthogonal axis in CHALLENGE.
- **Syntax fidelity: implement through to the semantics in v1** — correctly implement empty-value reset, scalar last-wins, list-accumulation structure, and the start-of-line-only comment rule. `config-file` is "recognized and warned (explicitly marking it unimplemented)."

## CHALLENGE — Verdicts and rulings (Magi + Void + Ripple, 2026-07-02)

### Consensus rulings (3-agent agreement — ADOPTED)

1. **Parser placement = pure-function separation (axis D adopted)**: separate the parser body as `&str -> Result<Vec<Directive>, ...>` (I/O-less), consumed by `noa-config`, following the existing `parse_overrides(path, source)` separation pattern and the norms of noa-vt's Handler/Stream. [Magi 3-0, conf 90]
2. **The noa-native path stays a single path**: Ghostty's 4-candidate merge is a resolution mechanism for Ghostty-specific historical baggage (bundle-id changes, `.ghostty` extension migration), and bringing it into noa itself is excessive. Keep the existing `default_config_path()` family, but **change the filename to a new name (`config`) that doesn't collide with `config.toml`** (structurally avoiding the "silently falls back to the default" regression once TOML is retired). [Magi 3-0 conf 85 / Void CUT conf 90]
3. **Faithfully implement discovery on the Ghostty side (the read source) as read-all-4-candidates, later-wins merge** (when import/reference is exercised) — this is precisely reproducing "Ghostty's observable behavior." [Magi 3-0, conf 85]
4. **Unknown keys = warn + continue**: copied real-world dotfiles are guaranteed to contain unsupported keys, so without this the JTBD fails entirely at the front door (unable to start over a single unknown key). This is a survival condition. [Void conf 95]
5. **A generic `--<key>=<value>` CLI is CUT**: speculative generalization for just 3 keys. `--cols`/`--rows` are kept unrenamed (renaming noa-specific keys with no Ghostty counterpart to Ghostty names would be misleading). **`--font-size` already matches the Ghostty name via clap's kebab-casing** (a Ripple finding — the renaming issue is effectively moot). [Magi 3-0 conf 78 / Void conf 90]
6. **TOML migration = a single detection warning + no auto-conversion**: detecting the old `config.toml` triggers one startup warning. Auto-conversion is not implemented, since it would contradict the L0 constraint of "unifying on one parser." [Magi 3-0 conf 80 / Void 7a KEEP 75, 7b CUT 90]
7. **The 10×4 minimum clamp is adopted** (a semantic change from reject → clamp); the `window-save-state` interaction needs no handling (noa has no equivalent feature). [Magi 3-0 conf 80 / Void 6b CUT 90]
8. **`config-file` is recognized with a dedicated warn** (not lumped into the generic unknown-key warn — so users can distinguish a typo from "recognized but not yet implemented"). Cycle detection and actual reading are deferred to a later increment. [Void KEEP 75]
9. **Diagnostic accumulation is kept outside the structs**: rather than adding a Vec to `StartupConfig`/`ConfigOverrides`, split it out via a `load_startup_config() -> Result<(StartupConfig, Vec<Diagnostic>)>` shape → **the `Copy` derive is preserved for v1**, keeping theme-selection's BLOCKER-1 (loss of Copy) from occurring until the theme key is actually added, in dependency order. Mimicking a Ghostty-style Diagnostic type / severity taxonomy is CUT (noa has no GUI dialog consumer for it). [Ripple mitigation 2 / Void 4c CUT 80]
10. **A generic list-accumulation data structure is DEFERRED** (a narrowing within the EXPAND ruling): since v1 has zero consumers of list-type keys, encountering a list-type key is handled like `config-file` — "recognized and warned" — with the real storage built in whichever increment first implements a list-type key. Scalar last-wins + empty-value reset + start-of-line comment rule are fully implemented in v1 (per the EXPAND ruling). [Void 8a CUT/DEFER 80 — finalized by tacit user consent]

### Provisional rulings ⚠ → **all items confirmed by the user (2026-07-03)**: ⚠A/⚠B/⚠C adopted as below, ⚠D's premise is confirmed to have occurred (theme-selection DONE → this spec is the next increment + the theme spec is revised), ⚠E (v1 recognition of the theme key) is adopted.

*(What follows is the record from when the provisional ruling was presented — the user was away from the keyboard at the 2026-07-02 checkpoint presentation)*

- **⚠ A = (b) flag + first-run hint**: explicit execution via the `--import-ghostty-config` flag (unsupported keys are written out commented-out) + a one-line usage hint shown when there's no noa config but a Ghostty config is detected. No automatic writing. [Adopted from Magi #1 3-0 conf 88 + Ripple mitigation 5. Magi #2's majority auto-import and Void's full CUT are recorded as dissenting verdicts]
- **⚠ B = warn + continue**: an invalid type value resets just that key to the default and startup continues (matches actual Ghostty behavior, keeps the error model uniform). [Adopted from Void 90 + Magi Logos minority opinion. Magi's majority (fail-fast) is recorded as a dissenting verdict]
- **⚠ C = "both required" applies only at the config layer**: the config keys `window-width`/`window-height` are invalid + warn if only one is specified (Ghostty semantics). The CLI `--cols`/`--rows` remain independently specifiable as noa-specific keys. [A synthesis of the Magi and Void proposals]
- **⚠ D = ghostty-config first**: implement this spec → revise the relevant section 7 of theme-selection.md (locked → re-locked) → kick off theme's orbit loop, in that order. [Ripple mitigation 1]
  - **[2026-07-03 premise broke — discovered during SPECIFY]** theme-selection's orbit loop is **already running / implementation in progress** (noa-config already has a `theme` field, `parse_theme`, and 2 tests; bin/noa has forwarding logic; noa-app already has `AppConfig.theme` added. The `Copy` derive has already been lost as well). The actual order has effectively reversed to **(b) theme-selection first**. L1/L2 have been reconciled against the actual code (R-8).
  - **[2026-07-03 addendum] The theme-selection loop is DONE (15/15 verified, per the Orbit row in .agents/PROJECT.md)**. The theme feature has **shipped** via TOML `theme = "name"`. → new issue **⚠E**: if this spec's R-8 stays as-is (theme treated as an unknown key), there will be a window between the ghostty-config implementation and the theme re-revision increment during which **the shipped theme feature stops working**. Since `ConfigOverrides.theme` and its downstream wiring already exist, the added cost of having the Ghostty syntax parser treat `theme` as a v1-recognized scalar key (string pass-through + a dedicated warn rejecting the `light:`/`dark:` pair syntax) is nearly zero. **Recommendation: include theme as a v1-recognized key to avoid a feature regression** (if adopted, revise R-8 accordingly, and rewrite theme-selection.md's R-1/R-2/AC-1–3 to assume Ghostty syntax). Requires a user ruling at LOCK time.

### Draft dissenting verdicts (record)

- **⚠ A. Whether an import mechanism is needed, and its form** (the mechanism for realizing the FRAME "both" ruling):
  - (a) **automatic import at first run** (non-destructive, one-time only, only when there's no noa config) [Magi 2-1, conf 62 — Pathos/Sophia majority]
  - (b) **`--import-ghostty-config` flag + a first-run hint display** (no automatic writing) [Magi #1 3-0 conf 88 + Ripple mitigation 5]
  - (c) **CUT the import mechanism entirely** — since the syntax is identical, `cp` plus one line of documentation can substitute [Void conf 75-80]
  - Note: (c) substantially shrinks the FRAME "both" ruling. Also, a raw-copy workflow invites a **warning flood** of unsupported keys (Flux #4), so an aggregated warning display ("N unsupported keys ignored (see log for details)") pairs with it.
- **⚠ B. Handling of invalid type values**:
  - (a) keep v1 fail-fast (document the gap) [Magi 2-1, conf 58 — concern that a real mistake could go unnoticed if buried in a log]
  - (b) **warn + continue (default that key only)** [Void conf 90 + Magi Logos minority + FACT (Ghostty continues)] — avoids the awkward middle ground (called out by Flux #3) of "unknown key warns but a type typo aborts startup"
- **⚠ C. "window-width"/"window-height" both-required**:
  - (a) don't adopt — keep independently settable (document the fidelity gap) [Magi 3-0, conf 80]
  - (b) adopt — a VERIFIED observable behavior, a few lines to implement [Void KEEP 80]
  - Synthesis: **only the config-key layer adopts Ghostty semantics (both required); the CLI `--cols`/`--rows` (noa-specific) stay independent** — reconciling fidelity with avoiding regression.
- **⚠ D. Execution order relative to theme-selection.md (locked)** (Ripple's top risk): theme spec's R-1/R-2, its L2 noa-config section, AC-1–3, and BLOCKER-1 all assume the `toml_edit`/`SUPPORTED_KEYS` machinery that this spec removes. Without settling the order before the orbit loop launches, double rework results.
  - (a) **ghostty-config implementation first** → revise theme-selection's relevant 7 sections (locked → re-locked) → then launch the orbit
  - (b) theme-selection implementation first (staying on TOML) → this spec migrates the theme key along afterward

### Ripple impact analysis (summary)

- Risk 5.5/10 = **MEDIUM (Conditional Go)**. Directly modifies 2 files (`noa-config/src/lib.rs` a full 342-line rewrite + a partial change to `bin/noa/src/main.rs`) + the workspace Cargo.toml (the `toml_edit` dependency, whose sole direct consumer is noa-config, can be removed — though it won't disappear from Cargo.lock, since it's still pulled in transitively via muda's build dependencies; don't be misled by that).
- **noa-app is unchanged** (AppConfig fields stay cols/rows/font_size; key-name mapping is absorbed at the single point in main.rs).
- Of the 10 existing tests, **about 7 need behavior-level rewrites** (`unknown_key_is_rejected`'s expectation flips entirely; `invalid_file_value_*` depend on the clamp/non-fatal rulings). 3 tests (defaults/merge/font_size NaN) are kept as-is.
- Estimated 400–600 lines → PR splitting is recommended (parser part / validation part / import part).
- Implementation begins after this spec's L1 is finalized (rulings A–D affect the scope of impact).

### Issues brought into CHALLENGE

1. **Import launch mechanism**: noa has no subcommand infrastructure (the binary is flags-only — the same constraint that led to `+list-themes` being DEFERRED in the theme spec). A `--import-ghostty-config` flag? Automatic import at first run? A new subcommand infrastructure?
2. **Designing the "both" experience**: without running the import, Ghostty assets never get used (a weakness of B). Auto-import + notification when there's no noa config but a Ghostty config exists?
3. **noa-native path scheme**: should the noa version faithfully reproduce Ghostty's read-all-4-candidates-and-merge (`~/.config/noa/config[.noa]` + App Support)? Or a single path?
4. **Diagnostic model**: how far to reproduce Diagnostic accumulation + parse-continuation. noa has no GUI error-dialog infrastructure → is v1 stderr/log only (document the fidelity gap)? Are invalid type values also made non-fatal?
5. **CLI flags**: how to handle `--cols`/`--rows` (rename to `--window-width` etc. / keep own names + internal mapping / implement a generic `--<key>=<value>`).
6. **window-width/height semantics**: faithfully adopt "both required, clamp to 10×4" (a behavior change, since current noa allows cols alone)?
7. **TOML migration UX**: warn when an existing config.toml is detected? Should import also handle TOML→new-format conversion? Do nothing?
8. **Parser placement** (axis D): a module inside noa-config vs. pure-function separation.

## L1 — Requirements

*(SPECIFY — Accord, 2026-07-03. Verified against the actual code — reflecting the parallel implementation of the theme-selection increment)*

### Functional Requirements

**Syntax parser foundation**

- **R-1**: implement a line-oriented parser for Ghostty syntax as a pure function with no file I/O, `parse_directives(source: &str) -> Vec<Directive>`. The syntax rules finalized for v1 are as follows.
  - Whitespace around the `=` in `key = value` is ignored.
  - A `#` comment is valid **only at the start of a line** (leading whitespace is allowed). A `#` appearing mid-value is not treated as starting a comment — it's kept as part of the value as-is (no trailing comments, FACT VERIFIED).
  - A non-empty line without an `=` produces no `Directive` and is silently skipped (no diagnostic is generated for it in v1 either). Parsing of other lines continues.
  - Line splitting is based on `str::lines()`; a trailing `\r` from CRLF line endings is stripped from the end of the value. A leading UTF-8 BOM is stripped before parsing. A non-UTF-8 file is not a Diagnostic but an I/O-lane error (see L2's "Two-lane error model").
- **R-2**: duplicate occurrences of a scalar key are **last-wins**. When the same key appears multiple times, the value that appears last in the source is used.
- **R-3**: if the text after `=` in `key =` is whitespace only (including empty), that key is treated as "unspecified" and resets to the default. On the other hand, an explicitly quoted empty string `key = ""` is not a reset — it's treated as a literal empty-string value (since all v1 scalar keys are numeric, this case is a type mismatch → enters the R-7 path).

**Key classification / warnings (unknown keys / list types / config-file)**

- **R-4**: a key that matches none of v1's recognized-key set (the 4 scalar keys `window-width`/`window-height`/`font-size`/`theme` [R-8, ⚠E], the 3 list-type keys from R-5 below, and `config-file` from R-6) **generates one unknown-key warn diagnostic and continues**. Parsing is not interrupted.
- **R-5**: explicitly limit v1's "recognized as a list-type key but value not retained" set to **`keybind` / `palette` / `font-family`**, three keys. These generate one warn diagnostic with dedicated wording distinct from R-4, and the value is skipped (no accumulation storage is implemented — per the EXPAND/CHALLENGE DEFER ruling).
- **R-6**: `config-file` is further distinguished from the list-type key set above, generating a warn diagnostic with wording distinct from both R-4 and R-5 (CHALLENGE ruling 8: distinguishing a typo from "recognized but not yet implemented"). Actual file reading, recursive include, and cycle detection are not performed.
- **R-7 ⚠B**: if a v1 scalar key (`window-width`/`window-height`/`font-size`) fails to parse as a number, or is numerically valid but out of semantic range (`font-size` non-positive, non-finite, etc.), **generate one warn diagnostic, fall back that key only to its default, and continue parsing**. This path applies **only to file-originated values** and is kept independent of `validate_startup_config`'s hard-fail path for the final CLI-originated value (the existing 2 CLI-oriented tests are unchanged).
- **R-8 ⚠E (revised by the 2026-07-03 user ruling — v1 recognition of the theme key)**: the theme-selection increment has **run to completion and shipped** (`ConfigOverrides.theme`/`StartupConfig.theme`/`AppConfig.theme` and its forwarding logic, accepting TOML `theme = "name"`). To avoid a feature regression, the Ghostty syntax parser accepts `theme` as a **v1-recognized scalar key (string)**: `theme = <name>` → `ConfigOverrides.theme = Some(name)` (quoting optional, following the R-1 rules; an empty value `theme =` is the R-3 reset). If the value has a `light:`/`dark:` prefixed pair syntax, **generate one warn diagnostic with dedicated wording and do not accept the value** (`theme == None` — partial acceptance, such as reading only one side, is forbidden. This carries over the theme-selection spec's Flux ruling of "silent fidelity divergence forbidden" into the warn+continue model). The TOML-only `parse_theme` implementation is removed, and the TOML-assuming tests `theme_key_is_accepted`/`light_dark_syntax_is_rejected` are rewritten to assume Ghostty syntax. theme-selection.md's R-1/R-2/L2 noa-config section/AC-1–3 are revised to assume Ghostty syntax as part of implementing this spec (per ⚠D).

**Diagnostics aggregation**

- **R-9**: `Diagnostic` is a lightweight type holding `message: String` (a Ghostty-style severity taxonomy is not mimicked — per CHALLENGE ruling 9), accumulated and returned as a `Vec<Diagnostic>` **outside** `StartupConfig`/`ConfigOverrides`. `load_startup_config`'s signature changes to `pub fn load_startup_config(cli: ConfigOverrides) -> anyhow::Result<(StartupConfig, Vec<Diagnostic>)>`. **(Correction based on the actual code)**: CHALLENGE ruling 9's premise of "preserving the Copy derive" has already broken down due to the theme-selection increment's earlier addition of `theme: Option<String>` (both structs are now `Clone`-only). Reinterpret the intent of this requirement as "don't let Diagnostics leak into the structs and add further complexity."

**noa-native path**

- **R-10**: the noa-native config **keeps the single-path scheme**, with only the filename changed from `config.toml` to `config` (`default_config_path()` returns `<config_dir>/noa/config`). The path-discovery / precedence (default < file < CLI) machinery itself is unchanged.

**window-width / window-height / font-size mapping**

- **R-11**: the `font-size` config key maps 1:1 to the internal `font_size: f32` field (the existing internal field names `cols`/`rows`/`font_size` are unchanged).
- **R-12 ⚠C**: `window-width`/`window-height` adopt Ghostty semantics (both required) **only at the config-file layer**. If only one is specified in the file, both are treated as unspecified (`None`), generating one warn diagnostic. If an empty-value reset (R-3) causes one to become unspecified, that's treated identically to "only one specified" (no tri-state is introduced to distinguish "reset" from "not present").
- **R-13 ⚠C**: when `window-width`/`window-height` are both valid numbers, `window-width` is clamped to a minimum of 10 and `window-height` to a minimum of 4. The clamp itself carries no diagnostic (following Ghostty's observable behavior — FACT VERIFIED).
- **R-14**: the CLI `--cols`/`--rows` (noa-specific keys) are outside the scope of R-12/R-13, remaining independently specifiable as before. They are not renamed.

**CLI**

- **R-15**: `Args`'s existing 3 fields (`cols`/`rows`/`font_size`) are unchanged in name and type. A generic `--<key>=<value>` flag is not implemented (already CUT as speculative generalization). `--font-size` doesn't need changes since it already matches Ghostty's key name via clap's kebab-casing.

**TOML migration**

- **R-16**: if the old `config.toml` (`<config_dir>/noa/config.toml`) is detected, generate **one warn diagnostic per startup run** (once per process, with no persistent "already shown" flag). The check depends only on the old file's existence, independent of whether the new `config` exists (so the warning fires as long as the old file remains, even after migration is complete, prompting deletion). No auto-conversion or auto-loading is performed.

**Ghostty import / first-run hint**

- **R-17 ⚠A**: `--import-ghostty-config` flag. On execution, reads whichever of the 4 Ghostty-side candidate paths exist — ①`ghostty/config.ghostty` under `$XDG_CONFIG_HOME` (falling back to `~/.config` if unset) ②same dir `ghostty/config` ③`~/Library/Application Support/com.mitchellh.ghostty/config.ghostty` ④same dir `config` — in that priority order, resolving with a **later-wins merge**. The v1-recognized scalar keys (`window-width`/`window-height`/`font-size`/`theme`, 4 keys) are written out to noa's `config` as their original line text unchanged; every other key (including list types, `config-file`, and truly unknown keys) is written out with the original line text preserved but prefixed with `# ` to comment it out. If a file already exists at the write target (`default_config_path()`), the write is **refused** (non-destructive, NFR-6). If none of the 4 candidates exist, this is treated as a failure.
- **R-18 ⚠A**: first-run hint. If noa's own `config` doesn't exist and at least one of R-17's 4 candidates exists, a normal startup (without the import flag) shows a **one-line** hint suggesting the use of `--import-ghostty-config`. No automatic writing is performed.

### Non-Functional Requirements (NFR)

- **NFR-1 (dependency hygiene)**: as part of the parser replacement, remove the `toml_edit` dependency from `noa-config`. No new external crate dependency is added (implemented using only the standard library's string processing). This restriction targets `[dependencies]`; `[dev-dependencies]` is out of scope for this NFR (though v1's tests are expected to be sufficient using `std::env::temp_dir()`, and no new dev-dep is expected to be needed either). Also remove the `toml_edit` entry from the workspace root `Cargo.toml`'s `[workspace.dependencies]`.
- **NFR-2 (quality gate)**: after this change, `cargo clippy --workspace` and `cargo test --workspace` must remain clean. No new `#[allow(...)]` added merely to silence warnings.
- **NFR-3 (dependency boundary)**: `noa-config`'s dependency graph must not include `wgpu`/`winit` (continuing the existing regression guarantee).
- **NFR-4 (offline build)**: `cargo build --workspace --offline` must succeed (no network access, no need to regenerate additional artifacts).
- **NFR-5 (determinism)**: the parser (`parse_directives`/the post-composition folding logic) is a pure function — it always returns the same output for the same input, and touches no file I/O, environment variables, or global mutable state. It remains in a form amenable to property-based testing (this doesn't require adding a new test dependency such as `proptest`). Purity is guaranteed by a mechanical check (a grep-based test, AC-45) that `src/parser.rs` contains no use of `std::fs`/`std::env`.
- **NFR-6 (non-destructiveness)**: `--import-ghostty-config` is non-destructive — it must never overwrite or modify an existing noa `config` file.

## L2 — Detail

Defines only the per-crate seams (no code is written).

### noa-config

The current `crates/noa-config/src/lib.rs` is **396 lines** (the "342 lines" cited earlier in this draft was a stale value predating the theme-selection increment's earlier changes — the line references in this L2 are verified against the actual code as of 2026-07-03).

**Module structure**

- `src/lib.rs` — holds `StartupConfig`/`ConfigOverrides`/`DEFAULT_*`/`load_startup_config`/`load_file_overrides`/`load_overrides_from_path`/`default_config_path`/`validate_startup_config`/`validate_grid_dimension`. TOML-detection diagnostics are appended here. **Two-lane error model (an intentional design decision)**: problems with parsed content (unknown key, type mismatch, a missing pair member, etc.) are all Diagnostics (warn + continue), while an actual file-read failure (non-UTF-8, permission error — a genuine I/O error) remains `anyhow::Result::Err` (fatal) as before. The former is meant to "not kill startup over a user config mistake," the latter to "not silently swallow a broken environment."
- `src/parser.rs` (new) — holds the `Directive`/`Diagnostic` types, the pure function `parse_directives`, the key-classification table, the folding logic `build_overrides`, and a thin wrapper `parse_overrides` that preserves the existing name (following noa-vt's Handler/Stream separation norm — CHALLENGE ruling 1).
  - `pub struct Directive { pub line: usize, pub key: String, pub value: Option<String> }` (`value: None` = empty-value reset)
  - `pub struct Diagnostic { pub message: String }` (no severity hierarchy — CHALLENGE ruling 9)
  - `pub fn parse_directives(source: &str) -> Vec<Directive>` (I/O-less, implements the R-1–R-3 syntax rules)
  - `fn build_overrides(path: &Path, directives: &[Directive]) -> (ConfigOverrides, Vec<Diagnostic>)` (implements the R-4–R-8/R-11–R-13 key classification, warnings, window-pair validation, and clamping)
  - `pub fn parse_overrides(path: &Path, source: &str) -> (ConfigOverrides, Vec<Diagnostic>)` — keeps the existing function name with a changed signature (dropping `anyhow::Result` in favor of a non-fatal model — matching Ghostty's "only OOM is fatal").
- `src/ghostty.rs` (new) — **split into "pure function + thin environment wrapper" for hermetic testability** (an Attest finding / an extension of CHALLENGE ruling 1): `pub fn ghostty_config_candidates_from(xdg_config_home: Option<&Path>, home_dir: &Path) -> [PathBuf; 4]` (a pure function, always returning all 4 paths regardless of existence) + a thin wrapper reading the real environment, `pub fn ghostty_config_candidates() -> [PathBuf; 4]` (shared by R-17/R-18). `$XDG_CONFIG_HOME` cannot be substituted by `dirs::config_dir()` (which returns the App Support directory on macOS), so the wrapper reads the environment variable directly, falling back to `~/.config`.
- `src/import.rs` (new) — similarly split: a pure part `pub fn build_import_output(source_texts: &[String]) -> (String, ImportStats)` (concatenation, classification, and commenting-out performed purely on strings, unit-testable) + an I/O part `pub fn import_ghostty_config_at(candidates: &[PathBuf], target: &Path) -> anyhow::Result<ImportOutcome>` (candidate reading, non-destructiveness check, writing) + a thin wrapper wiring up the real environment, `pub fn import_ghostty_config() -> anyhow::Result<ImportOutcome>` (details in the "Import writer" section).

**Replacement of existing elements**

| Current (lib.rs) | Replacement |
|---|---|
| `SUPPORTED_KEYS` (line 13) | the key-classification table inside `parser.rs` (v1's 3 scalar keys / 3 list-type keys / `config-file` / everything else = unknown). Classification only — not used as an allowlist for hard-failing |
| `reject_unknown_keys` (lines 126–137, hard fail) | inline classification + Diagnostic generation within `build_overrides` (continues, not fatal) |
| `parse_u16` (lines 139–158, based on toml_edit's `Item`) | a string-based numeric parse taking `Directive.value: Option<String>`. On failure, generates a Diagnostic + `None` instead of `bail!` (R-7) |
| `parse_font_size` (lines 160–176) | a string-based version of the same pattern (the positive/finite check is also made non-fatal via the same path) |
| `parse_theme` (lines 178–191) | **replaced** (R-8 ⚠E: the theme branch inside `build_overrides` — string pass-through + a dedicated warn for the `light:`/`dark:` pair syntax. The TOML-only implementation is removed) |
| `invalid_type` (lines 203–209, based on `Item::type_name()`) | a generic "invalid value" Diagnostic constructor with no type info (embeds only path, key, and raw text) |
| `find_first_existing_config_path` (lines 86–95) | **removed** (no callers — the noa-native path is a single fixed path per ruling 2, and the Ghostty side's 4 candidates use "read-all, later-wins merge," which is semantically different from first-wins and cannot be reused) |
| `default_config_path` (lines 82–84) | split into the pure function `default_config_path_in(config_dir: &Path) -> PathBuf` (filename `"config"`) + a thin wrapper `default_config_path()`. For old-file detection (R-16), add a parallel `legacy_toml_config_path_in`/`legacy_toml_config_path` (filename `"config.toml"`) |

**Existing elements that survive unchanged**

- `ConfigOverrides::merge` (lines 45–52) · `ConfigOverrides::apply_to` (lines 54–61) — the per-field `.or()`/`.unwrap_or()` logic is unchanged.
- `impl Default for StartupConfig` (lines 24–33) — the values of `DEFAULT_COLS`/`DEFAULT_ROWS`/`DEFAULT_FONT_SIZE` are also unchanged.
- `validate_grid_dimension` (lines 193–201) — logic unchanged (only the calling context changes).
- `validate_startup_config` (lines 117–124) — **its role is refined to being solely the final safety net for CLI-originated values**, while the logic stays unchanged and survives. Since file-originated invalid values are already neutralized (turned into None) by R-7, values that reach here are almost exclusively CLI-originated (the existing tests `validates_cli_grid_values_after_merge`/`validates_cli_font_size_after_merge` need no change).
- `load_overrides_from_path` (lines 97–101) — role preserved, only its return type changes to `anyhow::Result<(ConfigOverrides, Vec<Diagnostic>)>`.

**Change to `load_startup_config`**

Implement the real logic as `load_startup_config_from(config_path: &Path, legacy_path: &Path, cli: ConfigOverrides) -> anyhow::Result<(StartupConfig, Vec<Diagnostic>)>` with injectable paths, and change the public API `load_startup_config(cli)` (lines 64–70) into a thin wrapper wiring it to the real paths (for hermetic testability — an Attest finding). Internally, in addition to the Diagnostics from `load_overrides_from_path`, append the R-16 (TOML detection) Diagnostic before returning. The call to `validate_startup_config` (the hard-fail path) is preserved.

**`Cargo.toml` changes**

- `crates/noa-config/Cargo.toml`: remove `toml_edit.workspace = true` from `[dependencies]` (leaving only `anyhow`/`dirs`).
- root `Cargo.toml`: remove the `toml_edit` entry from `[workspace.dependencies]` (its sole direct consumer is `noa-config`; however it may still remain in `Cargo.lock` via muda's transitive build dependencies, so regression checks should be scoped to `cargo tree -p noa-config`).

### bin/noa

**Note**: `bin/noa/src/main.rs` is in the middle of changing from 32 lines → 66 lines due to the parallel theme-selection increment (an `app_config_from_startup` helper + 2 added tests already present). This section is described by **function name/structure** rather than absolute line numbers.

- add `#[arg(long)] import_ghostty_config: bool` to `Args` (its existing `cols`/`rows`/`font_size` unchanged) (`--import-ghostty-config`).
- **early branch for the import flag** at the top of `main()`, right after `Args::parse()`: if true, call `noa_config::import_ghostty_config()`; on success, print a summary (target path, supported/commented-out counts) to stdout; on failure, print the error to stderr; either way, **exit without launching the GUI (`noa_app::run`)**.
- normal path: change the return value of `load_startup_config(...)` to `let (config, diagnostics) = ...?;`, and output `diagnostics` one at a time **directly to stderr via `eprintln!`** (TOML detection, unknown key, list type, config-file, and invalid value are all output through this single uniform loop). **`log::warn!` is not used (Quality Gate F1)**: the current `env_logger::init()` sets the filter to `LevelFilter::Off` by default when `RUST_LOG` is unset (confirmed at vendored `env_filter-2.0.0/src/filter.rs:226`), so routing through `log` would make diagnostics completely invisible in the default environment. User-facing config diagnostics must not have their visibility depend on `RUST_LOG`.
- after diagnostic output, **the first-run hint check**: the decision logic is factored into the pure function `fn import_hint(config_exists: bool, any_candidate_exists: bool) -> Option<&'static str>` (unit-testable). `main()` passes in whether `default_config_path()` exists and whether `ghostty_config_candidates()` exist, and if `Some`, outputs one line **to stderr via `eprintln!`** (→ this resolves the Open Question about "where diagnostics are output to": the sink is stderr; no GUI dialog or new log-file infrastructure is added).
- **no change needed to main.rs for key-name mapping**: resolving `window-width`/`window-height`/`font-size` → the internal `cols`/`rows`/`font_size` completes entirely inside `noa-config`'s `build_overrides`. The existing `app_config_from_startup(config) -> AppConfig` keeps its field composition unchanged and survives unmodified.
- construction of `ConfigOverrides { cols: args.cols, rows: args.rows, font_size: args.font_size, theme: None }` on the CLI side is unchanged.

### noa-app

**No change from this spec.** The `theme` field on `AppConfig` (`crates/noa-app/src/app.rs:37-42`) belongs to the theme-selection increment; this spec is not involved. The only constraint ghostty-config imposes is "`cols`/`rows`/`font_size` must continue flowing through unchanged." The DAG boundary where `noa-app` doesn't depend on `noa-config` (only the binary bridges them) is also maintained.

### Import writer (`noa-config::import`)

- **API split (for hermetic testability)**: the pure part `build_import_output(source_texts: &[String]) -> (String, ImportStats)` performs all of concatenation, classification, and commenting-out on strings (unit tests can be string-only), while the I/O part `import_ghostty_config_at(candidates: &[PathBuf], target: &Path)` handles candidate reading, the non-destructiveness check, and writing. The no-argument `import_ghostty_config()` is a thin wrapper wiring up the real environment (`ghostty_config_candidates()`, `default_config_path()`).
- **Output format**: plain text, one line per Ghostty-syntax directive. No reformatting or re-serialization — **the original line text is output as-is** (preserving quoting, whitespace, and numeric notation).
- **Merge input**: reads whichever of R-17's 4 candidates exist, in priority order, concatenates the raw text with newline separators, then runs it through the same `parse_directives`. "Later-wins merge" is achieved via concatenation order + the existing scalar last-wins folding (no dedicated merge algorithm is introduced).
- **Comment-out rule**: classifies the key of each line in the concatenated source; supported lines (the 4 v1-recognized scalar keys) are written out unchanged, unsupported lines get a `# ` prefix. Blank lines and existing comment lines pass through untouched.
- **Handling of `config-file`**: not recursively tracked even during import. A `config-file = ...` line is simply subject to being commented out.
- **Write target / non-destructiveness**: always `default_config_path()` (the new name `config`). If a file already exists there, writes **nothing at all** and returns `Err` (NFR-6). Creates the parent directory if missing.
- **Zero candidates**: returns `Err` (a message listing the 4 paths) and writes nothing.
- **Open Question (undecided)**: whether to include an origin header comment (import source, date) remains undecided. Implementation may proceed in a minimal, headerless form, while keeping the design (line-based output) amenable to adding one non-destructively later.

### Test plan diff

The current `noa-config`'s `#[cfg(test)] mod tests` (lines 211–396) contains **12 tests** (the 10 counted at the time of the Ripple analysis + 2 added earlier by the theme-selection increment).

| # | Test name | Disposition |
|---|---|---|
| 1 | `defaults_match_existing_startup_behavior` | **kept** |
| 2 | `parses_supported_config_keys` | **rewritten** (TOML → Ghostty syntax `window-width=`/`window-height=`/`font-size=`, follow the tuple return value, also assert empty diagnostics) |
| 3 | `cli_overrides_config_file_values` | **kept** (calls `merge`/`apply_to` directly, parser-independent) |
| 4 | `theme_key_is_accepted` | **rewritten** (R-8 ⚠E: TOML syntax → verify acceptance of Ghostty syntax `theme = 3024 Day`) |
| 5 | `finds_first_existing_config_candidate` | **removed** (the function itself is removed) |
| 6 | `invalid_file_value_includes_path_and_key` | **rewritten** (since `cols = 0` now means "one member of the pair is missing," changed to verify a true type mismatch such as `window-width = abc` + warn+continue) |
| 7 | `invalid_type_includes_path_and_key` | **rewritten** (`font-size = large` as a non-numeric value: hard fail → verify warn+default-fallback) |
| 8 | `unknown_key_is_rejected` | **rewritten** (expectation flipped: hard fail → warn+continue. Verify `bogus-key = "x"` + continuation of other keys) |
| 9 | `light_dark_syntax_is_rejected` | **rewritten** (R-8 ⚠E: hard error → verify a dedicated warn diagnostic + non-fatal `theme == None`, assuming Ghostty syntax) |
| 10 | `invalid_file_values_are_rejected` | **rewritten** (`rows = 0` split off into the pair-missing case; `font_size = -1.0`/`inf` moved to warn+fallback) |
| 11 | `validates_cli_grid_values_after_merge` | **kept** |
| 12 | `validates_cli_font_size_after_merge` | **kept** |

**New tests added (representative, corresponding to the L3 ACs)**: start-of-line comment vs. non-comment `#` mid-value, skipping a line without `=`, quote stripping / preserving a single-sided quote, last-wins, empty-value reset / quoted empty string not being a reset, dedicated diagnostics for the 3 list-type keys, dedicated diagnostic for `config-file`, mutual distinctness of the 3 diagnostic wordings, `theme`'s generic-unknown-key path, one member of the window pair missing, the 9×4 boundary clamp, CLI `--cols` alone, TOML detection warn, import (zero/single/multiple-later-wins candidates, refusing an existing file, `config-file` not tracked), the 3 first-run-hint conditions, absence of `toml_edit` via `cargo tree -p noa-config`.

**bin/noa side**: the 2 existing tests from the theme-selection increment are out of scope for this spec and need no change. New tests are added for the import early-branch, the diagnostic-output loop, and the first-run-hint decision.

## L3 — Acceptance Criteria

Each AC states its corresponding `R-*`/`NFR-*` (in `AC-n → R-m` form). ACs depending on ⚠A/⚠B/⚠C carry the same mark.

### Syntax parser — basic rules

- **AC-1 → R-1**: Given `window-width   =   120` (extra whitespace around `=`). When `parse_directives` runs. Then `Directive{ key: "window-width", value: Some("120") }` (whitespace stripped).
- **AC-2 → R-1**: Given a comment line with leading whitespace, `  # a comment`. When `parse_directives` runs. Then no corresponding `Directive` is produced.
- **AC-3 → R-1**: Given `font-size = 14 # not a comment` (a `#` mid-value). When `parse_directives` runs. Then `Directive.value == Some("14 # not a comment")` (a `#` outside line-start position doesn't start a comment).
- **AC-4 → R-1**: Given a non-empty line with no `=`, `not-a-directive`, followed by `font-size = 15`. When `parse_directives` runs. Then no `Directive` is produced for the former, and the latter is correctly produced (parsing continues).
- **AC-5 → R-1**: Given `window-width = "120"` (a properly closed quote). When `parse_directives` runs. Then `Directive.value == Some("120")` (quote stripped).
- **AC-6 → R-1**: Given `window-width = "120` (an unclosed single-sided quote). When `parse_directives` runs. Then `Directive.value == Some("\"120")` (kept as a literal, routed through the AC-14 path downstream).
- **AC-7 → R-2**: Given `font-size = 14` followed by `font-size = 16`. When the whole source is parsed. Then `ConfigOverrides.font_size == Some(16.0)` (last-wins).
- **AC-8 → R-3**: Given `font-size = 14` followed by an empty-value `font-size =`. When the whole source is parsed. Then `font_size == None` (reset; after `apply_to`, `DEFAULT_FONT_SIZE`).
- **AC-9a → R-3**: Given `window-width = ""` (a quoted empty string). When parsed. Then `Directive.value == Some("")` (not the empty-value reset `None` — kept as a literal empty string).
- **AC-9b → R-3, R-7 ⚠B**: Given the same as above. When run through `build_overrides`. Then one type-mismatch diagnostic is generated and that key falls back to its default (the same path as AC-14).
- **AC-49 → R-1**: Given `key = "ab"cd"` (an unescaped `"` inside the value). When `parse_directives` runs. Then no quote stripping occurs — `value == Some("\"ab\"cd\"")` (kept as a literal).
- **AC-50 → R-1**: Given a CRLF-line-ending file (`font-size = 15\r\n`) and a file with a leading UTF-8 BOM (`\u{FEFF}font-size = 15`). When `parse_directives` runs. Then both produce `Directive{ key: "font-size", value: Some("15") }` (the `\r` and the BOM are stripped).

### Key classification / warnings

- **AC-10 → R-4**: Given a file containing `bogus-key = "x"` and `font-size = 15`. When `parse_overrides` runs. Then no error occurs; the diagnostics include one message referencing `bogus-key` and the file path, and `font_size == Some(15.0)` (parsing continues).
- **AC-11 → R-4, R-5, R-6**: Given an unknown-key diagnostic (AC-10), a list-type diagnostic (AC-12), and a `config-file` diagnostic (AC-13). When the 3 wordings are compared. Then all 3 are entirely distinct in wording, and the 3 categories are distinguishable.
- **AC-12 → R-5**: Given files each independently containing `keybind = "cmd+shift+f=..."`, `palette = "0=#000000"`, and `font-family = "Fira Code"`. When each is parsed. Then each produces one diagnostic and the value is not retained.
- **AC-13 → R-6**: Given `config-file = "~/.config/ghostty/extra"`. When parsed. Then one diagnostic is generated (wording distinct from AC-10/12). ("No file access occurs for that path" is structurally guaranteed by the parser's purity check, AC-45.)
- **AC-14 → R-7 ⚠B**: Given `font-size = not-a-number`. When parsed. Then no error occurs — `font_size == None`, and the diagnostics include one message referencing the path, `font-size`, and `not-a-number`.
- **AC-15 → R-8 ⚠E**: Given files with `theme = 3024 Day` (unquoted, containing whitespace) and `theme = "3024 Day"` (quoted). When `parse_overrides` runs. Then both produce zero diagnostics and `ConfigOverrides.theme == Some("3024 Day")` (accepted as a recognized scalar key, equivalent with or without quoting).
- **AC-51 → R-8 ⚠E**: Given `theme = light:Foo,dark:Bar`. When `parse_overrides` runs. Then no error occurs; a diagnostic with wording **distinct from both the unknown-key warn and the type-mismatch warn** is generated, and `ConfigOverrides.theme == None` (no partial acceptance of the pair syntax).

### Diagnostics aggregation

- **AC-16 → R-9**: Given a config file placed in a tempdir containing one unknown key and one type mismatch. When calling `load_startup_config_from(that config path, a nonexistent legacy path, ConfigOverrides::default())`. Then the return type is `anyhow::Result<(StartupConfig, Vec<Diagnostic>)>`, and the `Vec` contains 2 entries in file-occurrence order (hermetic — independent of the real home directory).
- **AC-17 → R-9**: Given the changed `StartupConfig`/`ConfigOverrides` definitions. When a unit test fully destructures both structs without `..`. Then neither has a `Diagnostic`-related field (an added field is caught as a compile error).
- **AC-18 → R-10**: Given an arbitrary base path. When calling `default_config_path_in(base)`. Then it returns `base/noa/config` (no extension, not `config.toml`). Verifiable environment-independently since it's a pure function.
- **AC-19 → R-10**: Given a `config` in a tempdir with `font-size = 16`, and a CLI-equivalent `ConfigOverrides { font_size: Some(18.0), .. }`. When `load_startup_config_from` runs. Then the final value is `18.0` (CLI > file preserved).
- **AC-20 → R-10**: Given a tempdir with neither `config` nor the old `config.toml`, and no CLI overrides. When `load_startup_config_from` runs. Then `Ok((StartupConfig::default(), vec![]))` (no error, empty diagnostics).

### Window sizing

- **AC-21 → R-11**: Given only `font-size = 15.5`. When parsed. Then `font_size == Some(15.5)` (not subject to pairing or clamping).
- **AC-22 → R-12 ⚠C**: Given only `window-width = 120` (`window-height` unset). When parsed. Then both `cols == None` and `rows == None` (both discarded), one diagnostic.
- **AC-23 → R-12 ⚠C**: Given the symmetric case (`window-height` only). When parsed. Then likewise both are `None` + one diagnostic.
- **AC-24 → R-13 ⚠C**: Given `window-width = 9` and `window-height = 4`. When parsed. Then `cols == Some(10)` (clamped), `rows == Some(4)` (unchanged, exactly at the floor).
- **AC-25 → R-13 ⚠C**: Given `window-width = 120` and `window-height = 30`. When parsed. Then `cols == Some(120)` · `rows == Some(30)` (unchanged).
- **AC-43 → R-7 ⚠B**: Given `window-width = abc` and `window-height = 30` (width non-numeric). When parsed. Then no error occurs — one type-mismatch diagnostic, width becomes `None`, and the result follows the pair-missing (R-12) treatment. The symmetric case `window-height = abc` behaves the same.
- **AC-44 → R-13 ⚠C**: Given `window-width = 120` and `window-height = 2` (height below the floor of 4). When parsed. Then `cols == Some(120)` · `rows == Some(4)` (height clamped).
- **AC-46 → R-3, R-12 ⚠C**: Given `window-width = 120` and `window-height = 30`, followed by an empty-value `window-height =` (an explicit reset). When the whole source is parsed. Then this is treated identically to "only one specified" — both become `None` + one diagnostic (there's no tri-state distinguishing "reset" from "not present").
- **AC-26 → R-14**: Given a CLI-equivalent `ConfigOverrides { cols: Some(50), rows: None, .. }`, with no config in the tempdir. When `load_startup_config_from` runs. Then no error occurs — `cols == 50` · `rows == DEFAULT_ROWS` (CLI is exempt from the both-required rule).

### CLI

- **AC-27 → R-15**: Given the `Args` definition. When inspecting the source or `noa --help`. Then only `--cols`/`--rows`/`--font-size` (unchanged) and the new `--import-ghostty-config` are present, with no generic `--<key>=<value>` mechanism.

### TOML migration

- **AC-28 → R-16**: Given a tempdir containing an old `config.toml` (with arbitrary old-TOML content) but no new `config`. When `load_startup_config_from` runs once. Then the diagnostics contain **exactly one** old-file-detection message, and the old file's content is neither parsed nor applied.
- **AC-47 → R-16**: Given a tempdir containing **both** an old `config.toml` and a new `config`. When `load_startup_config_from` runs. Then the new `config`'s content is applied, and the old-file-detection message is still included (the check doesn't depend on whether the new config exists).

### Ghostty import

- **AC-29 → R-17 ⚠A**: Given that none of the 4 candidate paths exist in the tempdir. When `import_ghostty_config_at(candidates, target)` runs. Then `Err` (a message enumerating the 4 candidate paths), with no file written to `target`. (The exit-code wiring for `noa --import-ghostty-config` is confirmed via the AC-27 flag-existence check + a one-shot review.)
- **AC-30 → R-17 ⚠A ⚠E**: Given one of the tempdir candidates containing `window-width = 100` · `theme = "Foo"` · `keybind = "cmd+n=new_tab"` · `window-decoration = false`, with `target` absent. When `import_ghostty_config_at` runs. Then `target` outputs `window-width = 100` and `theme = "Foo"` unchanged (recognized scalar keys), while the `keybind`/`window-decoration` lines keep their original text but get a `# ` prefix as comments, and returns `Ok`.
- **AC-31 → R-17 ⚠A**: Given a lower-priority tempdir candidate with `font-size = 12` and a higher-priority one (the App Support-equivalent slot) with `font-size = 14`, both present. When `import_ghostty_config_at` runs → reading the output `target` back via `parse_overrides`. Then `font_size == Some(14.0)` (later-wins merge).
- **AC-32 → R-17, NFR-6 ⚠A**: Given an existing file at `target`. When `import_ghostty_config_at` runs. Then `Err` (an "overwrite refused" message), and the existing file is byte-for-byte unchanged.
- **AC-33 → R-17 ⚠A**: Given a candidate with a `config-file = "<a real file inside the tempdir>"` line. When `import_ghostty_config_at` runs. Then that line is only commented out, and the content of the referenced file never appears in the output at all (no recursive tracking — verified mechanically by confirming no content leakage).

### First-run hint

- **AC-34 → R-18 ⚠A**: Given `config_exists == false` and `any_candidate_exists == true`. When calling the pure function `import_hint(config_exists, any_candidate_exists)`. Then it returns `Some(...)` whose text mentions `--import-ghostty-config`. (The stderr `eprintln!` wiring in main.rs and the fact that "no write occurs" are confirmed via a one-shot code review — the normal startup path requires the GUI, so a headless process test is not possible: a known constraint per CLAUDE.md.)
- **AC-35 → R-18 ⚠A**: Given `config_exists == false` and `any_candidate_exists == false`. When calling `import_hint`. Then `None`.
- **AC-36 → R-18 ⚠A**: Given `config_exists == true` (either case, regardless of candidate existence). When calling `import_hint`. Then `None`.

### Dependencies / quality

- **AC-37 → NFR-1**: Given the changed `crates/noa-config/Cargo.toml`. When inspected. Then `[dependencies]` contains only `anyhow` · `dirs`, and the root `Cargo.toml`'s `[workspace.dependencies]` has no `toml_edit` entry either.
- **AC-38 → NFR-1**: Given the changed workspace. When running `cargo tree -p noa-config --offline`. Then the output shows no `toml_edit` (a full `Cargo.lock` grep is not used, due to muda's transitive false positive — `-p noa-config` scoping is the source of truth).
- **AC-39 → NFR-2**: Given the finished change. When running `cargo test --workspace --offline` and `cargo clippy --workspace --offline`. Then both exit with code 0, with no new `#[allow(...)]`.
- **AC-40 → NFR-3**: Given `noa-config`'s dependency graph. When running `cargo tree -p noa-config --offline`. Then neither `wgpu` nor `winit` is included.
- **AC-41 → NFR-4**: Given a clean target directory. When running `cargo build --workspace --offline`. Then it succeeds with no network access and no need to regenerate artifacts.
- **AC-42 → NFR-5**: Given `parse_directives` and `build_overrides`, and a representative set of inputs (normal, with diagnostics, empty, several boundary cases). When each input is passed in twice. Then both runs return equal results (`PartialEq`) (machine-verifiable).
- **AC-45 → NFR-5, R-6**: Given the source of `crates/noa-config/src/parser.rs`. When grepping for uses of `std::fs`/`std::env` (and `dirs::`) (via a unit test's `include_str!` grep, or a CI step). Then none appear at all (structurally guaranteeing the parser's I/O-lessness — the verification mechanism for the "no access occurs" claims in AC-13/AC-33).
- **AC-48 → R-4, R-16, R-18 (visibility)**: Given the diagnostic-output and hint-output implementation in `bin/noa/src/main.rs`. When the source is inspected. Then the output is via direct `eprintln!` calls, with no path going through `log::warn!`/`log::info!` (structurally guaranteeing that diagnostics remain visible in the default environment even when `env_logger`'s filter defaults to `Off` with `RUST_LOG` unset — Quality Gate F1).

### Traceability summary

| Requirement | AC | Requirement | AC | Requirement | AC |
|---|---|---|---|---|---|
| R-1 | AC-1–6, AC-49, AC-50 | R-11 | AC-21 | NFR-1 | AC-37, AC-38 |
| R-2 | AC-7 | R-12 | AC-22, AC-23, AC-46 | NFR-2 | AC-39 |
| R-3 | AC-8, AC-9a, AC-9b, AC-46 | R-13 | AC-24, AC-25, AC-44 | NFR-3 | AC-40 |
| R-4 | AC-10, AC-11, AC-48 | R-14 | AC-26 | NFR-4 | AC-41 |
| R-5 | AC-11, AC-12 | R-15 | AC-27 | NFR-5 | AC-42, AC-45 |
| R-6 | AC-11, AC-13, AC-45 | R-16 | AC-28, AC-47, AC-48 | NFR-6 | AC-32 |
| R-7 | AC-9b, AC-14, AC-43 | R-17 | AC-29–33 | | |
| R-8 | AC-15, AC-30, AC-51 | R-18 | AC-34–36, AC-48 | | |
| R-9 | AC-16, AC-17 | | | | |
| R-10 | AC-18–20 | | | | |

All 24 target requirements (R-1–R-18, NFR-1–NFR-6) have ≥1 corresponding AC. R-7 covers type mismatches across all numeric scalar keys (AC-9b/14/43), R-13 covers clamping on both axes (AC-24/44), and R-8 covers theme's acceptance, pair-syntax rejection, and import pass-through (AC-15/51/30) at the content level (Quality Gate F4/F11 + addressed by ⚠E). Total ACs: 52.

## Scope

*(SHAPE — Spark, 2026-07-02)*

### 1. Problem

noa currently reads only `noa-config`'s TOML parser (allowlist-based, hard-fails on unknown keys) and cannot interpret Ghostty-native syntax (line-oriented `key = value`) config files at all. dotfiles-driven users migrating from Ghostty want to reuse their existing Ghostty config assets as-is (including semantics such as list accumulation via repeated keys, empty-value reset, and start-of-line comments), but with the current TOML-only parser and its "unknown key means instant death" behavior, copying real-world dotfiles fails to start over a single unsupported key the moment it's tried — the JTBD doesn't even survive the front door. On top of that, the theme-selection spec depends on this TOML-assuming machinery (`SUPPORTED_KEYS`, `toml_edit`), so unless the config foundation is replaced at the syntax level first, rework will ripple through every subsequent key-expansion increment.

### 2. Proposed solution

Separate `noa-config`'s parser portion as an I/O-less pure function interpreting Ghostty syntax (`&str -> Result<Vec<Directive>, ...>`), following the existing `parse_overrides(path, source)` separation pattern and the norms of noa-vt's Handler/Stream. Correctly implement the semantics through scalar last-wins, default-reset via empty value, and the start-of-line-only comment rule in v1, while list-type keys (`keybind`/`palette`/`font-family`, etc.) and the `config-file` directive are handled by being "recognized and given a dedicated warn" (using different wording from the generic unknown-key warn, so users can tell a typo apart from "recognized but not yet implemented" — actual storage and include reading are deferred to the next increment). Keep the noa-native config's single-path scheme, but change the filename to a new name, `config`, that doesn't collide with `config.toml`, structurally avoiding the regression where settings silently fall back to defaults after TOML is retired. Both unknown keys and invalid type values are warn + continue, with Diagnostics accumulated outside the config structs in the shape `load_startup_config() -> Result<(StartupConfig, Vec<Diagnostic>)>`, avoiding further complicating the config structs (note: the `Copy` derive has already been lost due to the theme-selection increment's earlier addition of `theme: Option<String>` — see the R-9 correction). `window-width`/`window-height` adopt Ghostty semantics (both required, with a shortfall being warn + ignore, and a 10×4 minimum clamp) only at the config layer, while the CLI `--cols`/`--rows` (noa-specific keys) keep their existing independent settability. Bringing in Ghostty assets is done not via a live fallback but via **explicit execution of the `--import-ghostty-config` flag** (a noa-specific convenience extension, with no Ghostty counterpart); only when it runs, it faithfully implements Ghostty's actual path-resolution rule (read all 4 candidates, later-wins merge), writes out supported keys in noa's format, and comments out unsupported keys to make them visible. When there's no noa config but a Ghostty config is detected, startup shows only a **one-line first-run hint** (also a noa-specific extension) pointing to this flag, with no automatic writing. When the old `config.toml` exists, a one-time detection warning is shown at startup; auto-conversion is not implemented since it would contradict the L0 constraint of "unifying on one parser."

### 3. In-scope

- Separate the Ghostty syntax parser as an I/O-less pure function inside `noa-config` (roughly `&str -> Vec<Directive>`), making unit tests writable without file I/O
- Syntax semantics implemented for v1: `key = value` (whitespace around `=` ignored), start-of-line-only `#` comments, scalar last-wins, empty value `key =` resets to the default
- The `config-file` directive: recognized with a dedicated warn (wording distinct from the generic unknown-key warn). Cycle detection and actual reading are deferred to the next increment
- List-type key syntax (accumulation from repeated keys): recognized and handled with a dedicated warn. Implementation of a generic list-accumulation data structure is DEFERRED to the next increment (v1 has zero consumers)
- noa-native config: preserve the existing path-discovery machinery (the `default_config_path()` family, single path, precedence: default < file < CLI), changing only the filename from `config.toml` to `config`
- Unknown keys: warn + continue (a behavior reversal from hard fail). The existing test `unknown_key_is_rejected` is rewritten to reflect "an unsupported key warns and startup continues"
- Invalid type values: warn + continue (only the affected key falls back to its default, startup continues) ⚠B
- Move Diagnostic accumulation outside `StartupConfig`/`ConfigOverrides`, returned in the shape `load_startup_config() -> Result<(StartupConfig, Vec<Diagnostic>)>` (avoiding a Vec leaking into the config structs — `Copy` has already been lost as of the theme-selection increment, so the goal here is "no further complication" rather than "preservation." R-9)
- `window-width`/`window-height` (config keys): both required (either alone is invalid + warn), with a 10×4 minimum clamp adopted ⚠C. The CLI `--cols`/`--rows` remain independently specifiable as noa-specific keys as before, with no renaming
- `--font-size` CLI flag: no change needed (already matches the Ghostty name `font-size` via clap's kebab-casing)
- One-time detection warning at startup when the old `config.toml` is found (no auto-conversion)
- `theme` config key (⚠E adopted 2026-07-03): continue accepting the shipped theme feature under Ghostty syntax (string pass-through; the `light:`/`dark:` pair syntax gets a dedicated warn and is not accepted — no partial acceptance). Revising theme-selection.md's relevant section to assume Ghostty syntax is included in this implementation increment
- **`--import-ghostty-config` flag (a noa extension — not fidelity) ⚠A**: faithfully implement Ghostty's actual path-resolution rule at execution time (4 candidates: `$XDG_CONFIG_HOME/ghostty/{config.ghostty,config}` and `~/Library/Application Support/com.mitchellh.ghostty/{config.ghostty,config}`, later-wins merge), reading all of them and writing out supported keys in noa's `config` format. Unsupported keys are left in the output as comments so the user can review and migrate them manually
- **First-run hint (a noa extension — not fidelity) ⚠A**: if there's no noa config and a Ghostty config is detected via the same 4-candidate search, display a one-line hint on how to use `--import-ghostty-config`. No automatic writing

### 4. Out-of-scope

- **Automatic TOML conversion** — directly contradicts the L0 constraint of "unifying on one parser" (a noa-specific scope decision, not a fidelity gap)
- **Automatic import at first launch (without a flag)** — the ⚠A ruling adopted the flag approach, avoiding the side effect of writing a file with no user action (not a fidelity gap — Ghostty has no import feature of its own)
- **Generic `--<key>=<value>` CLI** — CUT as speculative generalization for just 3 keys (Ghostty accepts every config key as a CLI flag, so this is **documented as a fidelity gap**)
- **Actual reading of `config-file` (recursive include, cycle detection, end-of-file deferred processing)** — kept at recognize+warn (**documented as a fidelity gap**)
- **Implementing accumulation storage for list values** — kept at recognize+warn (**documented as a fidelity gap**)
- **Live config reload (`cmd+shift+,` / SIGUSR2)** — keeps the current read-once-at-startup architecture (**documented as a fidelity gap**, an existing item on parity-plan Phase 3)
- **GUI "Configuration Errors" dialog** — noa has no GUI dialog infrastructure; diagnostics are stderr/log only (**documented as a fidelity gap**)
- **New subcommand infrastructure** — import is realized via a flag, keeping the binary flags-only (the same constraint judgment as the theme spec's `+list-themes` DEFER)
- **`window-save-state` interaction** — noa has no corresponding feature (**documented as a fidelity gap** for the missing feature)
- **Semantic expansion of list-type keys such as `keybind`/`font-family`/`palette`** — a separate spec / separate increment (`theme` was promoted to a v1-recognized scalar key by the ⚠E ruling [2026-07-03] — R-8)

### 5. Assumptions

- v1's target keys are the syntax foundation + 4 recognized scalar keys (`window-width`/`window-height`/`font-size`/`theme` [⚠E]). Expansion to list-type keys like `keybind`/`font-family` is a separate spec / separate increment
- The noa-native config's path-discovery machinery (single path, precedence model) is unchanged. Only the filename (`config.toml` → `config`) and the parser portion change
- The Ghostty-side 4-candidate read-all-and-merge is used only "when `--import-ghostty-config` runs" and "at detection time for the first-run hint display" — noa never reads Ghostty config live during normal startup (a consequence of direction B)
- `noa-app` maintains the existing DAG boundary of not depending on `noa-config`, with key-name mapping absorbed at the single point in `bin/noa/src/main.rs`. `AppConfig`'s fields (cols/rows/font_size) are unchanged (a Ripple premise)
- Implementation order follows the ⚠D ruling (**though its premise broke as of 2026-07-03 — see the CHALLENGE section addendum**): the original ruling was "implement this spec → revise theme-selection → launch theme's orbit," but since theme-selection's orbit loop is already in progress, the actual order has effectively reversed to (b) theme-selection first. L1/L2 have been reconciled against the actual code (the theme field already added). The final order is confirmed by the user at LOCK time

## Considered but rejected

EXPAND checkpoint (2026-07-02, user ruling): **direction B (Faithful Import) adopted alone**.

- **A. Fast Path (minimal parser + live fallback)** — rejected: real dotfiles become "readable but semantically incomplete," causing rework of the syntax machinery in later increments. Live fallback also carries the warn flood of unsupported keys (Flux #4).
- **C. Include Bridge (implicit `config-file` injection)** — rejected: "implicit include" is a noa-specific composition behavior with no Ghostty counterpart, in tension with the faithful-clone philosophy.
- **E. Layered Merge (always merging 2 files)** — rejected: deviates from the FRAME ruling of "only when absent," worsens the debugging experience of a 2-file precedence, and is a concept that doesn't exist in Ghostty.
- **Fully automatic import at first launch** — rejected (⚠A provisional): a side effect of writing a file with no user action. This was Magi 2-1's majority proposal, but Ripple mitigation 5's flag + hint approach was adopted instead. Recorded as a dissenting verdict.
- **Fully CUT the import mechanism (cp + docs only)** — rejected (⚠A provisional): Void conf 75-80. Not adopted since it substantially shrinks the FRAME "both" ruling. Recorded as a dissenting verdict.
- **Keeping fail-fast for invalid type values** — rejected (⚠B provisional): Magi 2-1's majority proposal. warn+continue was adopted instead, to avoid the awkward middle ground (named by Flux #3) of "unknown key warns but a type typo aborts startup." Recorded as a dissenting verdict.
- **Porting the 4-candidate merge to noa's own path** — rejected: this resolves Ghostty-specific historical baggage (bundle-id changes, extension migration) that noa has no equivalent of. [Void CUT 90]
- **Mimicking a Ghostty-style Diagnostic type / severity taxonomy** — rejected: noa has no GUI dialog consumer for it. [Void 4c CUT 80]
- **Automatic TOML→new-format conversion** — rejected: would keep the to-be-removed old parser alive, contradicting "unifying on one parser." [Magi 3-0 / Void 90]

## Open Questions / Deferred Decisions

**Record of finalized rulings (approved by the user 2026-07-03)**

- **⚠A finalized**: the `--import-ghostty-config` flag + a first-run hint (no automatic writing). The dissenting verdicts (auto-import / full CUT) are recorded under Considered but rejected.
- **⚠B finalized**: invalid type values are warn + continue (R-7).
- **⚠C finalized**: the both-required rule applies only at the config layer; the CLI `--cols`/`--rows` remain independent (R-12/R-14).
- **⚠D finalized**: given that the theme-selection loop is DONE, this spec is the next implementation increment. Revising theme-selection.md's relevant sections (R-1/R-2/L2 noa-config/AC-1–3) to assume Ghostty syntax is included in the implementation increment.
- **⚠E finalized**: include `theme` as a v1-recognized scalar key (R-8 revised, AC-15/51 reflected).

**Other unresolved items**

- Whether to leave an origin header comment (import source, date) in the noa `config` written by import — a traceability concern, undecided
- The trigger condition for the next increment that moves `config-file`/list-type keys from "recognize+warn" to actual reading — undecided
- Whether the noa-native config filename `config` should carry an extension (Ghostty added the `.ghostty` extension as a precedent in 1.2.3+) — this increment proceeds on the assumption of no extension
- Display format for unsupported-key warnings — per-key individual lines vs. an aggregated display ("N unsupported keys ignored") — Flux #4, undecided
- The concrete output destination for startup diagnostics (stderr only / also a log file) — excluding a GUI dialog is finalized; the format of the alternative output was finalized during SPECIFY

## Spec Quality Gate record

- **Run 1 (2026-07-03, Judge + Attest)**: **GATE FAIL**.
  - Judge blocking: F1 (assuming diagnostic output goes through `log::warn!` makes it invisible due to `env_logger`'s default filter of `Off` — confirmed against actual code) / F2 (the "Copy preserved" statement in Scope contradicts the R-9 correction) / F3 (the ⚠D premise breakdown wasn't reflected in Assumptions) / F4 (missing AC for window-related type mismatches in R-7). Non-blocking: F5–F12.
  - Attest: LOCK-ready NO — 13/51 ACs are non-hermetic due to argument-less API shapes (`default_config_path()`/`ghostty_config_candidates()`/`import_ghostty_config()`), and the hint-output sink was undefined.
- **Revision (2026-07-03)**: addressed all blocking issues and the major non-blocking ones — finalized the output sink as stderr `eprintln!` (F1, AC-48) / corrected the 2 Copy-related statements in Scope (F2) / added a note to Assumptions about the ⚠D premise breakdown (F3) / added AC-43·44 (F4, F11) / clarified in R-12 that reset and unspecified are treated identically + added AC-46 (F5) / added the embedded-quote rule + AC-49 (F6) / clarified the R-16 trigger condition + AC-47 (F7) / refined AC-42 to determinism only, moving purity to the AC-45 grep check (F8, Attest#7) / documented the two-lane error model (F9) / added the CRLF/BOM rule + AC-50 (F10) / documented key expansion under Out-of-scope (F12) / split the L2 API into "pure function + thin wrapper" (`default_config_path_in`, `ghostty_config_candidates_from`, `build_import_output`, `import_ghostty_config_at`, `load_startup_config_from`, `import_hint`) and rewrote AC-16/19/20/26/28–36 into hermetic form (Attest #1–6) / clarified NFR-1's dev-dependency scope (Attest #9).
- **Run 2 (2026-07-03, independent verification)**: **GATE PASS**. Confirmed in the actual text that all 4 Judge blocking items + 8 non-blocking items + all Attest blockers are FIXED. No ghost references or gaps in the traceability table (51 ACs). The only remaining note was about the heading placement of AC-43/44/46/49/50 (→ now reformatted, moved into their respective thematic sections).
- **LOCK prerequisites satisfied (2026-07-03)**: ⚠A–E confirmed by the user + sign-off obtained. Consistency updates for R-8/AC-15/AC-30/AC-51/test plan/Scope tied to the ⚠E adoption are complete (total ACs: 52).

## Build-path decision

**orbit loop (engine: codex)** — chosen by the user at LOCK time, 2026-07-03.

- This spec's 52 L3 ACs serve as the completion contract (a machine-checkable DONE gate) for a runner generated by orbit as part of `nexus-autoloop`. Same build-path (orbit/codex) as theme-selection.
- Prerequisites for running Codex (`~/.codex`'s `multi_agent = true` + `[agents] max_depth >= 2`, `-o` artifact capture) must be confirmed before starting.
- Ancillary work included in the implementation increment: revising theme-selection.md's relevant sections (R-1/R-2/L2 noa-config section/AC-1–3/the Copy-derive paragraph/the Open Questions path descriptions) to assume Ghostty syntax (per ⚠D).
</content>
