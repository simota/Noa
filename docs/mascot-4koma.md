# Noa Mascot — Engineer 4-koma Scripts

Recurring 4-panel strips: **relatable engineer moments ("we’ve all been there")** starring
Noa and her companion Ember. The engine is the **gap**: Noa stays flat and
unbothered while Ember carries all the emotion.

## Format & voice

- **Layout**: 2×2 quadrant grid (square). Reading order (EN, left→right):
  **top-left → top-right → bottom-left → bottom-right**. Beat structure:
  **setup → build → turn → punchline** (bottom-right).
- **Clarity rule (important)**: the **bottom-right panel must state the payoff in
  words** — never leave the joke to a silent "…". Panel 1 must make the situation
  obvious (what she's doing, what she wants). Keep lines short, but complete enough
  to land without the reader already knowing the meme.
- **Noa**: flat, deadpan, unbothered. Tells: hood up = embarrassed; sweater-paws =
  tired (see `mascot-ip-bible.md` §1).
- **Ember**: the reaction engine — the screams, panic, and cheers Noa won't show.
  Ember usually delivers the loud punchline; Noa delivers the dry one.
- **Art**: ready-to-generate one-paste 2×2 prompts live in `mascot-4koma-prompts.md`.

---

## Strips

Layout key: **TL** top-left · **TR** top-right · **BL** bottom-left · **BR** bottom-right.

### #1 — It Works On My Machine
*Relatable: your tests pass, CI fails, and the "fix" is absurd.*
- **TL** `[Noa's laptop showing all-green passing tests]` Noa: "all green here."
- **TR** `[Ember pointing in alarm at a big CI wall-screen, all red ✗]` Ember: "CI is ALL red…!"
- **BL** `[Noa, flat, small shrug]` Noa: "works on my machine."
- **BR** `[Ember shoving Noa's laptop into a shipping box]` Ember: "…then we ship your machine?!"

### #2 — Rubber Duck
*Relatable: saying the bug out loud solves it — the "listener" did nothing.*
- **TL** `[Noa turns to Ember, serious]` Noa: "let me explain the bug to you."
- **TR** `[Ember puffs up proudly, listening like a good rubber duck]` Ember: "mm-hm!"
- **BL** `[Noa lights up mid-sentence, already turning away]` Noa: "…oh. fixed it. thanks!"
- **BR** `[Ember alone, deadpan, an empty chair beside it]` Ember: "…I said nothing."

### #3 — The One-Character Fix
*Relatable: three hours of debugging, and the fix was one character.*
- **TL** `[Noa wrecked and hollow-eyed, a 3:00 clock behind her]` Noa: "found the bug."
- **TR** `[extreme zoom on code: one missing semicolon glowing orange]`
- **BL** `[Noa, flat]` Noa: "…one character."
- **BR** `[Ember erupting, arms up]` Ember: "THREE HOURS FOR ONE CHARACTER?!"

### #4 — The Five-Minute Estimate
*Relatable: "five minutes" is a unit of hope, not time. (identical framing each panel.)*
- **TL** `[Noa, hands in pocket, glancing at a ticket; window in daylight]` Noa: "give me five minutes."
- **TR** `[same framing; window at sunset]`
- **BL** `[same framing; window at night; Ember asleep, mugs piling]`
- **BR** `[same framing; window at sunrise; Noa still typing, dark circles]` Noa: "…five more minutes."

### #5 — git blame
*Relatable: "who wrote this garbage" → it was you, six months ago.*
- **TL** `[Noa scowling at code in disgust]` Noa: "who wrote this garbage?"
- **TR** `[Ember eagerly running the command, sparkles]` Ember: "git blame~!"
- **BL** `[screen, big and legible: "Author: Noa — 6 months ago"]`
- **BR** `[Noa yanking her hood down over her face]` Noa: "…forget I asked."

### #6 — --force
*Relatable: force-push nukes the team's work — but git never truly forgets.*
- **TL** `[Noa's finger over Enter on `git push --force`, calm]` Noa: "it's fine."
- **TR** `[the screen flashes; the team's commits wiped; dead silence]`
- **BL** `[Ember melting down, hands to face]` Ember: "you deleted EVERYONE'S work!!"
- **BR** `[Noa calmly typing, the commits restoring]` Noa: "git never forgets."

### #7 — Just One More Feature
*Relatable: "one more thing before bed" loops until sunrise.*
- **TL** `[cozy dark room, warm-orange screen glow]` Noa: "one more feature, then bed."
- **TR** `[Ember yawns, curls up asleep on the keyboard]`
- **BL** `[a clock spinning, the mug pile growing]`
- **BR** `[sunrise; Ember wakes to find Noa in the exact same pose]` Noa: "…one more feature."

### #8 — The Heisenbug (the log that can't be removed)
*Relatable: adding a log hides the bug — so the "temporary" log ships forever.*
- **TL** `[Noa glaring at a glitchy crash on screen]` Noa: "why does it crash?"
- **TR** `[she adds a `print()` line; the bug vanishes]` Noa: "add a log… it stops?"
- **BL** `[she deletes the `print()`; the bug pops back, smug]` Noa: "remove it… it's back."
- **BR** `[Noa leaves the log in forever, flat; the crash stays gone]` Noa: "…the log stays."

---

## Extending the series (more relatable moments to script)

Naming things (`data` → `data2` → `dataFinalReal`) · "no blockers" at standup (has
many) · the quick refactor that touched 40 files · TODO: fix later (never) · works
in dev, dies in prod · the solution arriving in the shower · off-by-one · deleting
`node_modules` as therapy · the Friday 5 PM incident.

**Punchline engine (reuse every time)**: Noa stays flat → Ember overreacts → the
bottom-right panel **says** the ironic payoff out loud (Ember shouts it, or Noa
deadpans it). Keep her tells (hood, sweater-paws) as visual support, never as the
whole joke.

#TODO(agent): pick a run of ~4 strips for the first post batch (dialogue locked:
short EN, payoff stated; layout locked: 2×2 grid).
