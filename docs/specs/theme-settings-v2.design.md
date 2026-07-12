# theme-settings-v2 Technical Design (Atlas / lightweight MADR)

- **status:** proposed (design firming before implementation loop begins)
- **target spec:** `docs/specs/theme-settings-v2.md` (R-19–R-34 / NFR-7–9 / AC-25–51)
- **design principles:** minimal structural change · preserve the existing 34+6+11 tests · R-12 commit-order invariant · dependency rules (only noa-app touches winit; only noa-render/noa-app touch wgpu) · GPU gotchas unchanged
- **surveyed source files:** `theme_settings/state.rs` `rows.rs`, `macos_overlay/sync.rs` `model.rs`, `app/render.rs`, `app/sidebar/palette.rs`, `app/input_ops/theme_settings.rs`, `app/config.rs`, `noa-config/src/writer.rs` `lib.rs`, `debounce.rs`, `session.rs`

---

## Overall picture (decision summary)

| # | Question | Decision (one line) |
|---|------|------|
| ADR-1 | F1 snapshot type | Instead of a new type, wrap `filtered`/`available_font_families` in `Arc` so the existing `clone()` becomes O(1). Both rendering paths' signatures stay unchanged. |
| ADR-2 | F2 lightweight key | Place a `ThemeSettings::view_fingerprint(&mut Hasher)` on the state side that does not assemble the ViewModel, checking it against ADR-1's `Arc` pointer identity plus the favorites generation. Idempotent frames build the ViewModel zero times. |
| ADR-3 | F3 fuzzy | Don't adopt timer debounce; use prefix-differential narrowing exclusively. Forward typing is immediate (scanning only within the previous set); a full rescan of all 574 happens only on prefix-breaking edits. |
| ADR-4 | AC-13 pair layer | Generate the pair string inside `commit_updates()` (since AC-49 directly verifies this, this is the only viable placement). Pass a resolved `ThemePairContext` through `ThemeSettingsInit`. Writer unchanged. |
| ADR-5 | Threading new elements through the 3 sync points | Don't unify the rendering models; add pure leaf functions of the same shape as `settings_row_display_value` to the `theme_settings` module, shared by both paths. Favorites persistence and toast generalization also fall under this. |

---

## ADR-1 — F1: build the rendering snapshot via `Arc` wrapping, not a new type

### Context
`App::redraw` (`app/render.rs:44-48`) duplicates the entire `ThemeSettings` every frame via `session.state.clone()`. What `#[derive(Clone)]` (`state.rs:80`) deep-copies is mainly `filtered: Vec<ThemeMatch>` (up to 574 entries, each with a `Vec<usize>`) and `available_font_families: Vec<String>` (the font list, tens to hundreds of entries). This duplicated value is passed as `&ThemeSettings` to both the macOS path (`sync_theme_settings` → ViewModel construction) and the non-macOS path (`draw_theme_settings_card` → ANSI text). The reason the clone is needed is stated explicitly by the existing doc comment (`state.rs:71-79`): "cut out to an owned value early, since the borrow of `App::theme_settings` cannot be held across the `&mut self` calls in the latter half of redraw."

In practice, both rendering paths only read a **windowed slice** of `filtered` (native = `THEME_LIST_ROWS` 8 entries, wgpu = `LIST_ROWS` entries, both centered on highlighted). The fuzzy match's `positions` are discarded by both paths (`palette.rs:783` `let Some((name, _positions))`, `model.rs:190` `.map(|(name,_)|`). In other words, there's no need to hold all 574 entries owned in full.

### Considered Options
1. **Introduce a new "thin, rendering-only snapshot type"** (per the literal wording of spec R-19). Build a `ThemeSettingsSnapshot { mode, section, badge, filter, windowed_themes: Vec<(String,bool)>, swatches, rows: [(label,value,restart);16], commit_error, count, contrast, ... }` via a builder, and change both rendering functions' signatures from `&ThemeSettings` to `&ThemeSettingsSnapshot`.
2. **Wrap `filtered` and `available_font_families` in `Arc`**, degrading `clone()` into a reference-count bump. Both rendering functions stay `&ThemeSettings`.
3. Stop cloning and extend the lifetime of the borrow (restructure redraw).

### Decision
**Option 2.** Change the 2 fields of `ThemeSettings` to
```
filtered: Arc<Vec<ThemeMatch>>,
available_font_families: Arc<Vec<String>>,
```
`recompute_filtered` fully replaces via `self.filtered = Arc::new(filter_themes(...))` (already never mutates in place, always fully replaces, so the semantics are unchanged). `session.state.clone()` at `render.rs:44-48` stays the same text, but its cost drops from O(574) to O(1) plus small-field duplication (filter string, 2 Strings in a row, 2 Strings in `RevertValues` — about 5 small allocs total). Both rendering functions, all accessors, and the ViewModel builder are unchanged.

### Rationale
- Satisfies AC-25's measurement condition, "no full duplication including `filtered: Vec<ThemeMatch>`, substantially less duplicated data than before," with a **minimal diff (about 10 lines plus accessor return-value adjustments)**.
- Option 1 is faithful to the spec's literal wording, but since the two paths' windowing capacity (8 vs `LIST_ROWS`) and offset algorithm (`overlay_scroll_window` vs `saturating_sub(list_rows/2)`) differ, folding both into a single windowed snapshot would require merging one path's windowing policy into the other's, cascading into both builders in `palette.rs`/`model.rs`, both signatures, and even F2's redesign (~150 lines, extensive test changes). Since this is a single-user local app where the overlay is only ever open transiently, this investment isn't warranted by any additional measurement gain (AC-25 is already satisfied by Option 2).
- **Synergy with ADR-2**: `Arc<Vec<ThemeMatch>>`'s `Arc::as_ptr` identity represents "has the filtered set changed" in O(1). This gets the core of F2's lightweight key for free. This is the biggest reason Option 2 was chosen.

### Rejected
- Option 1 (new type): high churn plus double-windowing debt, no gain beyond AC-25. `state.rs:71-79`'s doc comment foreshadows a "follow-up zero-copy type," but given the constraint of cutting the borrow, owned-ification is required regardless, and `Arc`'s cheap-copy is the practical zero-copy equivalent.
- Option 3 (extend borrow): conflicts with the `&mut self` calls in the latter half of redraw (`sidebar_draw_model`, etc.) and cannot pass the borrow checker. This is exactly why the existing doc comment chose to clone.

### Consequences
- (+) Minimal diff, unchanged signatures, catalyzes F2's key. (+) The per-frame duplication of `available_font_families` also disappears at the same time (a side benefit not mentioned in the spec).
- (−) Deviates from spec R-19's literal wording of "a new rendering-only snapshot type" → recorded here as an intentional deviation (AC-25 is satisfied by measurement).
- **Fitness**: No `#[cfg(debug_assertions)]` clone counter is needed (the deep-copy path itself disappears with Arc-ification). AC-25 is expressed as a unit test (no GPU needed) demonstrating that `ThemeMatch` is not deep-copied, via the increase in `Arc::strong_count` (sharing).

---

## ADR-2 — F2: place a `view_fingerprint` that does not assemble the ViewModel on the state side

### Context
`sync_theme_settings` (`sync.rs:61-83`) unconditionally builds `theme_settings_view_model(state)` **before** the hash comparison (line 69), and builds it **again** if there was a change (line 80). ViewModel construction allocates 8 windowed `String` clones, plus `noa_theme::resolve`, `sample_swatches` (the swatch array), 16 rows' display-value `String`s, and the footer `String`, every time. R-20/NFR-7 requires "zero constructions on an idempotent frame" and "zero missed changes" (AC-26/AC-27).

### Considered Options
1. **A monotonic `revision: u64` counter**, bumped at each mutator site, keyed on `(revision, rect, colors)`.
2. **`ThemeSettings::view_fingerprint(&mut impl Hasher)`** implemented on the state side. Directly hash the raw fields that affect the ViewModel (`Arc::as_ptr(filtered) as usize`, mode, section, filter, highlighted, selected_row, highlight_moved, commit_error, the 16 rows' (draft, touched), edit buffer length, favorites generation, attribute_filter). sync hashes the fingerprint plus rect plus colors as the key, and builds the ViewModel **only once** on change, passing it into rebuild.
3. Keep the status quo, only eliminating the double construction (build once on change, still once on idempotent).

### Decision
**Option 2.** Place `view_fingerprint` as a method on `ThemeSettings`, **right next to the field definitions**. `sync_theme_settings` becomes:
```
let key = model.map(|(state, rect)| hash_u64(|h| { state.view_fingerprint(h); rect.hash_into(h); colors.hash_into(h); }));
if cache.theme_settings == key { return; }
cache.theme_settings = key;
let vm = theme_settings_view_model(state);   // once, only on change
imp::rebuild_theme_settings(window, model.map(|(_,r)|(vm, r)), colors);
```
Idempotent frame: ViewModel constructed 0 times (fingerprint is O(16), no allocs). Changed frame: constructed once (double construction eliminated).

### Rationale
- **Structurally guarantees zero missed changes (AC-27)**: every input to the ViewModel is either (i) a raw field that directly enters the fingerprint, (ii) a value purely derived from `filtered` (identity via `Arc` pointer) plus highlighted (the windowed list, swatches, count, contrast, R-33 samples), or (iii) a row display purely derived from the 16 rows' draft, selected_row, and section. All inputs map onto the fingerprint.
- **Suppresses false negatives via colocation**: keeps the knowledge of "what affects the ViewModel" alongside the state, rather than in sync.rs. A future field-adder is expected to update the fingerprint right next to it (the same responsibility locality as deriving `Hash`).
- Option 1 (revision) has its bump sites scattered across 8+ mutators, and any new mutator that forgets to bump immediately causes a false negative. This is fragile under the "zero missed changes" obligation.
- `Arc::as_ptr` identity tracks set change since ADR-1 fully replaces `filtered` (never mutates in place). Since a favorites toggle can change the star decoration without changing the set, a separate `favorites_epoch: u64` is included in the fingerprint (below, ADR-5).

### Rejected
- Option 1: risk of missed bumps in scattered locations.
- Option 3: idempotent-frame construction doesn't disappear, failing to meet NFR-7/AC-26.

### Consequences
- (+) `#[derive(Hash)]` on ViewModel is **kept** (for other uses/regression insurance) but is taken off the sync hot path. (+) `f32` fields in draft are hashed via `to_bits()` (contained within the fingerprint).
- **Fitness (permanent guard for AC-27)**: property-test that "iterating over every mutator, whenever the ViewModel changes, the fingerprint must also change" (asserting simultaneity between `view_fingerprint` and `theme_settings_view_model` diffs before/after each `move_*`/`push_text`/`adjust`/favorites operation). Kept in CI.

---

## ADR-3 — F3: fuzzy is prefix-differential narrowing only, no timer debounce

### Context
`push_text`/`backspace` (`state.rs:384-437`) unconditionally runs `recompute_filtered` → `filter_themes` (a full scan of all 574 entries, applying `fuzzy_match` to each) on every keystroke. Spec R-21 lists (a) debounce coalescing and (b) differential narrowing within the previous set upon prefix extension. NFR-8/AC-28/AC-29's **measurement target is scan scope** (prefix extension → previous `filtered` set; prefix break → all 574).

### Considered Options
1. **Prefix-differential plus timer debounce combined**: forward typing is an immediate differential; full rescans on prefix breaks (Backspace/replacement) are coalesced via `Debouncer<String>`.
2. **Prefix-differential only (no debounce)**: forward typing = immediate rescan of only the immediately-preceding `filtered` subset; prefix break = immediate full rescan of all 574.
3. **Debounce all edits uniformly** (only the trailing value fires).

### Decision
**Option 2.** Implement differential narrowing in the filter-edit path:
- When the new filter string is an extension of the old filter (`new.starts_with(&old)` and longer) → the scan target is only the name group in the immediately-preceding `Arc<Vec<ThemeMatch>>` (apply `fuzzy_match(new, name)` to the prior set).
- Otherwise (shortening, non-prefix replacement, or clearing) → fall back to a full rescan of all 574 via `filter_themes(new)`.
`Debouncer` remains dedicated to font-size only (`state.rs:96`); it is not introduced for the filter.

### Rationale
- AC-28's load-bearing claim is scan scope (only the first "3" scans all 574; subsequent ones scan only within the previous set). Differential narrowing satisfies this **without a timer**: "3" starts from 574 and narrows, while "30", "302", "3024" each scan only the **already-narrowed previous set**.
- AC-29's claim (prefix break → fall back to full scan) is also satisfied by the else branch of the differential check.
- Fuzzy matching against 574 entries (short strings) is empirically sub-millisecond. Forward typing's differential is even smaller. **Adding debounce would impose a 150ms delay on the first keystroke and on Backspace, degrading list responsiveness (i.e. visibility)** — given the spec's trade-off of "immediate first character vs. debounce," this design chooses the immediate side. At the 574-entry scale, there is no measurable gain from coalescing, so it's unneeded complexity (a `void`-style judgment).
- Combining with debounce (Option 1) creates a state where "the displayed list and the applied set diverge while pending," and also bifurcates the timing of preview (`should_preview`/`gpu.preview_theme`). Unnecessary complexity at the 574-entry scale.

### Rejected
- Option 1: no gain from the added complexity at the 574-entry scale, and unnecessary delay on forward typing.
- Option 3: first-character delay, contrary to the spec's preference for immediacy.

### Consequences
- (−) **AC-28's test implementation needs adjustment**: rather than "simulate debounce-window elapse," assert via "`fuzzy_match` call count (scan scope)" — a debug counter (`#[cfg(test)]` `AtomicUsize`, or recording the return value) that verifies "first call = 574, subsequent = previous length." This test-shape change is a legitimate consequence of this architectural decision.
- (+) Only one additional function beyond `filter_themes` is needed: a differential version `narrow_filtered(prior: &[ThemeMatch], filter)`. The existing single-matcher `fuzzy_match` contract is preserved.
- **Open**: only if future profiling shows that even the narrowed forward-typing scan is measurably heavy, retroactively add `Debouncer<String>` to the **widen (fallback) path only** (keeping forward-typing's immediacy). Not deemed necessary at this time.

---

## ADR-4 — AC-13: pair preservation generates the pair string inside `commit_updates()`

### Context
Under a `theme = light:X,dark:Y` setting, committing a single theme from the panel causes `commit_updates()` (`state.rs:806-812`) to produce `("theme", name)`, which `apply_updates` (`writer.rs`) unconditionally replaces the final `theme` line with — a single name — destroying the pair syntax. `apply_updates`'s contract is "replace the key's final line with `key = value`"; passing a pair string as `value` preserves the syntax (solvable without touching the writer, confirmed by inspection). Currently `open_theme_settings` (`input_ops/theme_settings.rs:43-49`) passes only `self.config.theme` (the already-resolved single name) and never `self.config.theme_appearance: Option<ThemeAppearancePair>` (`config.rs:23`, holding the raw pair value). Active-side determination follows the same shape as `self.system_appearance: winit::window::Theme` (`app.rs:126`) plus the existing `effective_theme_name` (`config.rs:363`).

**Decisive constraint**: AC-49 directly verifies that `commit_updates()`'s **return value** contains `("theme","light:C,dark:B")`. So the transformation must occur inside `commit_updates()`; an "App-layer preprocessing wrapper" (per the spec's L2 prose) cannot satisfy AC-49. **The AC takes priority over the spec prose** (in line with the spec metadata's permitted deviation).

### Considered Options
1. **Generate the pair string inside `commit_updates()`**. `ThemeSettings` holds a resolved pair context and, when emitting the theme diff, composes `light:_,dark:_` with the active side = new name and the other side = retained.
2. Post-transform `updates` at the App layer (retrofit `commit()` to take a transform, or extract `commit_updates()`).
3. The magi ruling's other option — a "present + explicit confirm" modal.

### Decision
**Option 1.**
- Add a pure context to `ThemeSettingsInit`:
  ```
  theme_pair: Option<ThemePairContext>,   // struct ThemePairContext { active_is_light: bool, light: String, dark: String }
  ```
  `open_theme_settings` resolves and passes this from `self.config.theme_appearance` and `self.system_appearance` (the winit `Theme` check happens on the App side; the pure module receives only a `bool`, preserving the dependency rule and testability).
- `ThemeSettings` holds `theme_pair: Option<ThemePairContext>`. `commit_updates()`'s theme branch:
  ```
  if let Some(name) = highlighted_theme_name(), name != snapshot.theme_name {
    match &self.theme_pair {
      Some(ctx) => {
        let (light, dark) = if ctx.active_is_light { (name, ctx.dark.as_str()) } else { (ctx.light.as_str(), name) };
        updates.push(("theme".into(), format!("light:{light},dark:{dark}")));
      }
      None => updates.push(("theme".into(), name.into())),   // existing behavior (regression protection for AC-51)
    }
  }
  ```
- `apply_updates` is unchanged.
- **In-memory sync follow-up**: after a successful commit, `commit_theme_settings` (`input_ops/theme_settings.rs:395-397`) currently does `self.config.theme = Some(name.1)`, but for a pair, `name.1` becomes `"light:C,dark:B"`, which would corrupt `self.config.theme` (assumed bare name). For a pair, add a branch that updates the active side of `self.config.theme_appearance` with the new name instead, without touching `self.config.theme` (in the spirit of R-34 — this is a separate layer of correctness from AC-49/50's write, for reopen consistency).

### Rationale
- Since AC-49 verifies the return value, the placement is a one-way choice of `commit_updates()` (Option 2 is unviable).
- Option 1 touches nothing in the writer contract, NFR-5's byte precision, or other-key behavior — the only change is the `value` passed in, becoming a pair string, satisfying NFR-9 with a minimal diff.
- Rationale for adopting "rewrite only the active side" (the latter of magi's two options): (a) adding a new modal is inconsistent with magi's interaction-cost-minimization policy, (b) it's always reversible via Esc/undo toast (R-31), (c) the pair decision fits into the existing touched model (Conservation Constraint 2) as a side effect, requiring no new UI state.

### Rejected
- Option 2: cannot satisfy AC-49 (return-value verification) / retrofitting `commit()` would harm the simplicity of R-12's failure-step handling.
- Option 3: a new modal violates the scope policy. AC-C2 (pair both-sides editing UI) is an **extension** on top of this, not a substitute.

### Consequences
- (+) One branch covers AC-49/50/51. AC-51 (`theme_pair=None` → `("theme","C")`) has no regression via the else branch.
- (−) `ThemeSettingsInit` gains a new field (mechanically appended across all 14 construction sites, see "Test fallout" below).
- **Fitness**: AC-50 is verified via a tempdir integration test round-tripping through `parse_theme_pair` plus asserting the inactive side is byte-identical (alongside the existing 11 writer tests).

---

## ADR-5 — new display elements, favorites, and toast: subordinate to the existing leaf-sharing pattern, no "unified model"

### Context (points 6 / 5 / R-31)
theme-settings has 3 rendering sync points: (1) wgpu `theme_settings_overlay_text` (`palette.rs:718`), (2) shared value formatter `settings_row_display_value`/`RowDraft::display_value` (`rows.rs:145-203`, called by both (1) and (3)), (3) native `theme_settings_view_model` (`model.rs:178`). Conservation Constraint 5 is "(2) must not be forked by either path." R-26 (count) / R-27 (contrast) / R-29 (star) / R-33 (multi-line samples) are new displays on the **theme list side**, but list rendering is currently assembled inline separately by (1) and (3), with no shared function.

### Decision
**Do not unify the rendering model.** Add **pure leaf functions** of the same shape as (2) to the `theme_settings` module, called by both (1) and (3):
- `match_count_label(highlighted: usize, total: usize) -> String` (R-26, e.g. `"3 / 12"`)
- `contrast_label(fg: Rgb, bg: Rgb) -> (String, bool)` (R-27, calls `noa_render::theme::contrast_ratio`; threshold 4.5 = `DEFAULT_MINIMUM_CONTRAST`)
- `attribute_of(theme_def) -> Attribute {Light,Dark}` (R-30, promotes noa-app's existing `theme.rs:119 relative_luminance` to `pub(crate)` for reuse. **noa-render unchanged**)
- `sample_lines(theme_def) -> Vec<SampleLine>` (R-33, reuses `sample_swatches`'s color data to generate text lines with the actual fg/bg/selection colors; (1) and (3) call the same function = AC-48)
- The star mark is drawn simply by ViewModel/overlay_text checking `favorites.contains(name)` inline (a simple branch, no leaf needed)

**Favorites persistence (point 5 / R-29):**
- **Location**: add `theme_favorites_path()` plus `theme_favorites_path_in(dir)` to `noa_config` (the same pub-fn-pair convention as `default_config_path`/`session_state_path`). **On the config-dir side**, `~/.config/noa/theme-favorites` (via `xdg_config_dir()`). Not session.json (data_dir, a topology that vanishes with `window-save-state=never`) — favorites are a UI preference that ought to persist, next to config.
- **Format**: plain newline-delimited text (1 line = 1 theme name). This repo doesn't use serde (session.rs is hand-written JSON). Favorites is just a `HashSet<String>`; line-delimited text needs no parser and is minimal.
- **Mechanism**: `App` holds `FavoritesStore { set: HashSet<String>, path: PathBuf }`. Lazily loaded on the first `open_theme_settings` (best-effort, falls back to an empty set on read failure, does not block startup — a spec edge case). Toggling writes atomically immediately (reusing the temp→rename flow from session.rs:363). Must not touch the commit path (`commit_updates`/`write`) at all (AC-40/41).
- **Feeding into the session**: `ThemeSettingsInit.favorites: Arc<HashSet<String>>` plus `favorites_epoch: u64`. Toggling mutates the store on the **App side** → swaps in a new `Arc` on the session and bumps the epoch (ADR-2's fingerprint picks this up to detect star changes). `filter_themes`/differential narrowing join favorites/attribute_filter as narrowing conditions (commit_updates stays unaffected).

**Toast generalization (R-31):**
- Turn `WindowState.resize_overlay: Option<(String, Instant)>` (`state.rs:316`) into `Option<Toast>`:
  ```
  struct Toast { text: String, until: Instant, kind: ToastKind }
  enum ToastKind { Resize, Undo(Box<RevertValues>) }
  ```
  Keep a single slot (spec edge case: a new toast immediately replaces the old). `sync_toast`/`draw_toast_card` only pass `toast.text` (rendering unchanged).
- Undo trigger: on successful commit, return the `RevertValues` snapshot from immediately before `commit` (the clone of the `state.rs:95` snapshot) to `App`, pushing a `ToastKind::Undo`. Undo execution re-commits the snapshot values using the **existing `write_config_updates` plus existing `apply_runtime_font_size`/`apply_live_*`** (R-31's constraint: "don't create a new mechanism"). Trigger key is a keybind (click is out of magi scope).

### Rationale
- A unified rendering model (folding both renderers into one model) is a mega-change that cascades into GPU gotchas / both signatures — excessive for a single-user app. Extending the existing pattern of "pure leaf functions shared by both paths" (already proven by `settings_row_display_value`) to the new elements is both the literal reading of Conservation Constraint 5 and the minimal approach.
- Placing favorites in the config dir reflects a judgment that this is "a preference, not a topology" (not erased by `window-save-state`). The pub-fn-pair convention directly supplies AC-41's tempdir injection point.
- The single-slot toast plus enum tag directly matches the spec edge case (new replaces old) without touching the rendering layer.

### Rejected
- Unified rendering model: mega-ADR, excessive abstraction.
- Piggybacking favorites onto session.json: erased by `never` setting, mixes with topology.
- Introducing serde or a custom JSON format: excessive for what's just a line-delimited set.

### Consequences
- (+) New elements are added purely via leaf functions, avoiding copy-paste hell across the 3 sync points (AC-47/48). (+) Favorites/attribute join only as narrowing conditions, keeping commit inviolate (AC-40).
- (−) `ThemeSettingsInit` gains `favorites`/`favorites_epoch`/`attribute_filter`/`carryover` (R-25)/`theme_pair` (ADR-4) → test fallout (below).
- **Fitness**: AC-48 is backed by code review (and, where possible, a test asserting a shared function reference) that "(1) and (3) call the same `sample_lines`/leaf function."

---

## Additional component decisions (sub-ADR / mechanical)

- **R-25 carryover**: `ThemeSettingsInit.carryover: Option<ThemeSettingsCarryover { filter: String, highlighted: usize, selected_row: usize }>`. Tab does not branch into either `close` (Esc) or `commit` (Enter) — as a **third transition** it takes the carryover from the current session and reconstructs the new session via `open_theme_settings(reverse mode, carryover)`. `gpu.preview_theme` and live-applied runtime values (font-size, etc.) are never touched (AC-36). `ThemeSettings::open` overrides the filter/highlighted/selected_row initialization with the carryover value when `Some`.
- **R-32 wheel**: At the top of `on_mouse_wheel` (`event_loop.rs:1124`), right after the `handle_sidebar_wheel` early return, add `if self.active_overlay(window_id)==ActiveOverlay::ThemeSettings { return self.handle_theme_settings_wheel(window_id, lines); }` (the same bool-consumption contract). Accumulation follows the `WHEEL_PAGE_THRESHOLD`-style pattern from `apply_overview_wheel`, mapping onto highlighted/selected_row movement (AC-45/46).
- **R-22/23/24 menu / keybindings**: Swap the dispatch body of `AppCommand::Preferences` (`app/commands.rs:66`) for `open_theme_settings(Settings)` (menu id/accelerator unchanged = AC-31). New `AppCommand::EditConfigFile` (retains the old `open_config_file()`, menu item + palette row, no default chord). `OpenThemePicker` gets a menu item plus default `cmd+shift+,` (added to `KeybindEngine::default()` specs).

## Handling of test fallout (14 `ThemeSettingsInit` construction sites)

All new Init fields default to `Option`/owned-empty (`theme_pair:None, carryover:None, favorites:Arc::new(HashSet::new()), favorites_epoch:0, attribute_filter:None`), so the 14 sites can be handled with **mechanical appends** (most are absorbed by the `init()/settings_init()/transparent_init()` helpers in `tests.rs`; only `rows.rs`/`input_ops` tests construct it directly). This is churn, not design risk. The existing 51 tests (`theme_settings/tests.rs` 34 + `input_ops/theme_settings.rs` 6 + `writer.rs` 11) are not broken in semantics by any decision here (Arc-ification = duplication semantics unchanged, fingerprint = new, commit_updates pair branch = follows the existing path when `theme_pair:None`).

## Dependency rules / conservation constraints check
- The winit `Theme` check happens only at the App layer (ADR-4). The pure `theme_settings` module receives only `bool`/owned values = stays GUI-independent and testable.
- noa-render unchanged (ADR-1/2/5). `contrast_ratio` is existing pub; `relative_luminance` is only a visibility promotion of an existing noa-app-internal function (does not touch noa-render).
- noa-config addition is only 2 path helper functions (fits the existing convention). Writer unchanged (ADR-4).
- R-12 commit order (config write → chrome swap) unchanged (ADR-4 only changes value generation, doesn't interfere with ordering).
- GPU gotchas: this design touches neither uniform layout nor bind-group visibility at all (only the rendering data path).

## Rollout / rollback
Land the 3 performance-remediation items (ADR-1→2→3) first (local, low-risk, regressions caught by existing tests), then ADR-4 (data safety, integration tests via tempdir first), and finally the ADR-5-family richness items (leaf additions plus favorites/toast). Each ADR is independently revertible (Arc-ification/fingerprint/pair-branch/leaf functions are loosely coupled). AC-C1/C2 can be dropped individually per spec, only when judged cheap.

## Next steps
Risk Gate (omen: FM-EA-family pre-mortem / ripple: fallout confirmation for the 14 `ThemeSettingsInit` sites, `commit_updates`, `sync.rs` / echo: UX of the Tab third transition and toast replacement). Implementation proceeds via the titan/builder loop.

---

## Amendments (Phase 5 Risk Gate — omen FMEA / ripple reflected, 2026-07-11)

### ADR-4 correction (FM-01, RPN=448 — precondition for implementation, must)
Fix the derivation of `current_theme` in `open_theme_settings`. Currently it only reads `self.config.theme`, so while a pair (`theme = light:X,dark:Y`) is active, it always falls through to `""`, causing a Settings-only commit's `commit_updates()` to emit a phantom theme diff that silently overwrites the pair. Fix: derive it using the same shape as `effective_theme_name(config, system_appearance)` (the active side's name for a pair, or `config.theme` otherwise), eliminating the path where `current_theme` becomes empty under a pair configuration. This is a **precondition** for adding `ThemePairContext`, not an independent add-on task. Special-casing an empty snapshot on the `commit_updates` side to swallow it is forbidden (it would break the initial theme-setting flow). As defense in depth, add an invariant test that "`highlight_moved` is always false in Settings mode."

### ADR-2 reinforcement (FM-02): upgrade the property test for view_fingerprint simultaneity across all mutators from nice-to-have to a required AC.
### ADR-3 addendum (FM-07): rather than reusing `apply_overview_wheel`'s `WHEEL_PAGE_THRESHOLD` value directly, introduce a dedicated separate constant for the wheel accumulation threshold.
### ADR-5 addendum (FM-08/FM-09): give undo re-commit a guard that "invalidates if another commit/reopen occurred since the toast was shown." Favorites file write failures must not be silent — warn-log plus, if possible, a one-line commit_error-equivalent notification.
### Correction to construction site count (ripple condition 1 / omen measurement): "14 sites" was off by one. omen measurement is 13 (tests.rs 10 + input_ops 3) plus 32 indirect sites via helpers (ripple). The Builder must mechanically recount at implementation start.
### carryover design change (FM-04): `ThemeSettingsCarryover` should include the `RevertValues` (at minimum theme_name) from the point of the very first open, carrying it through Tab round trips without re-fetching the snapshot. Esc always rolls back to the very first open point.
### Alignment of the two rendering-path strategies (FM-06): for the row-count strategy of the added chip row, "explicitly choose corresponding degradation strategies for wgpu/native, and confirm via a code-review AC adjacent to AC-48."
