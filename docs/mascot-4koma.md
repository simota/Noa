# Noa Mascot — Engineer 4-koma Scripts

Recurring 4-panel strips: **engineer "あるある" (relatable dev moments)** starring
Noa and her companion Ember. The engine is the **gap**: Noa stays flat and
unbothered while Ember carries all the emotion — the punchline lands in the space
between her calm and the disaster.

## Format & voice

- **Layout**: 2×2 quadrant grid (square). Reading order (EN, left→right):
  **top-left → top-right → bottom-left → bottom-right**. Beat structure:
  **setup → build → turn → punchline** (bottom-right).
- **Dialogue**: short, English, sparse. Many panels carry no line (SFX / caption /
  face only) — that's fine and reads cleaner.
- **Noa**: short flat lines, trailing "…", never panics. Tells: hood halfway up =
  embarrassed; sweater-paws = tired (see `mascot-ip-bible.md` §1).
- **Ember**: the reaction engine — the screams, panic, and cheers Noa won't show.
- **Art**: each panel = `[IP Common Block]` (`mascot-ip.md`) + the panel's `[visual]`.
  Ready-to-generate 2×2 prompts live in `mascot-4koma-prompts.md`.

---

## Strips

Layout key: **TL** top-left · **TR** top-right · **BL** bottom-left · **BR** bottom-right.

### #1 — It Works On My Machine
*あるある: CI is red, your screen is green, both are "true".*
- **TL** `[Noa at desk, flat; a wall screen glowing red with a big ✗ behind her]` Noa: "…passes here."
- **TR** `[Ember squinting up at the red ✗, one sweat drop]` Ember: "it's SO red."
- **BL** `[Noa gestures flatly at her all-green screen]` Noa: "not my bug."
- **BR** `[Ember lifts a laptop overhead like an offering; Noa stares a beat too long]` Ember: "…ship your laptop?"

### #2 — Rubber Duck
*あるある: you solve it the instant you explain it out loud.*
- **TL** `[Noa turns to Ember, dead serious, finger up]` Noa: "so the bug is—"
- **TR** `[Ember bolt upright, proud attentive nod]` Ember: "mm!"
- **BL** `[Noa's eyes widen a fraction; she's already standing, turning away]` Noa: "—oh. never mind."
- **BR** `[Ember alone, still nodding at empty air]` Ember: "…I helped?"

### #3 — The One-Character Fix
*あるある: three hours of debugging, and the fix was `=` → `==`.*
- **TL** `[Noa buried in glowing tabs, 3 AM clock, messy hair, sweater-paws]` caption: "hour 3."
- **TR** `[Ember face-down asleep, tiny snore]`
- **BL** `[extreme zoom on one code line: `=` → `==`, highlighted orange]` Noa: "…"
- **BR** `[Noa dead flat foreground; Ember behind, jolting awake mid-scream]` Ember: "THREE HOURS?!"

### #4 — The Five-Minute Estimate
*あるある: "five minutes" is a unit of hope, not time. (identical framing each panel.)*
- **TL** `[Noa, hands in pocket, glancing at a ticket; window in daylight]` Noa: "five minutes."
- **TR** `[same framing; window at sunset]`
- **BL** `[same framing; window at night; Ember asleep, mugs piling]`
- **BR** `[same framing; window at sunrise; Noa still typing, dark circles]` Noa: "…almost done."

### #5 — git blame
*あるある: "who wrote this garbage" → it was you, six months ago.*
- **TL** `[Noa reading code with quiet disgust]` Noa: "…who wrote this."
- **TR** `[Ember helpfully hits a key, sparkles of initiative]` Ember: "git blame!"
- **BL** `[screen: author — "Noa", 6 months ago]`
- **BR** `[Ember points at the screen; Noa yanks her hood halfway up, blushing]` Noa: "…nobody."

### #6 — --force
*あるある: the calm before `git push --force`, and the abyss after.*
- **TL** `[Noa's cursor hovering over a force-push button, finger raised, flat]` Noa: "…it's fine."
- **TR** `[the screen flashes white; dead silence; Noa expressionless]`
- **BL** `[Ember spiraling into horror, hands to face]` Ember: "our HISTORY—!!"
- **BR** `[Noa already calmly typing again; Ember collapsing in relief]` Noa: "…reflog."

### #7 — Just One More Feature
*あるある: "one more thing before bed" is a promise to the sunrise.*
- **TL** `[cozy dark room, warm-orange screen glow]` Noa: "one more, then bed."
- **TR** `[Ember yawns, curls up asleep on the keyboard]`
- **BL** `[a clock spinning, the mug pile growing]`
- **BR** `[sunrise; Ember wakes to find Noa in the exact same pose]` Noa: "…one more."

### #8 — The Heisenbug
*あるある: the bug vanishes the moment you add a print statement.*
- **TL** `[Noa glaring at a glitchy shadow-bug on screen]` Noa: "reproduce."
- **TR** `[she adds a glowing print line; the bug poofs to nothing]` Noa: "…gone."
- **BL** `[she deletes the line; the bug pops back, smug, arms crossed]`
- **BR** `[Noa's long flat stare; the bug ducks behind Ember, who shrugs]` Noa: "…"

---

## Extending the series (more あるある to script)

Naming things (`data` → `data2` → `dataFinalReal`) · "no blockers" at standup (has
many) · the quick refactor that touched 40 files · TODO: fix later (never) · works
in dev, dies in prod · the solution arriving in the shower · regex → now two
problems · off-by-one · deleting `node_modules` as therapy · the Friday 5 PM
incident.

**Punchline engine (reuse every time)**: Noa stays flat → Ember overreacts → the
bottom-right panel is the gap between her calm and the size of the disaster. Ember
gets no credit for the wins and all the panic for the losses. Keep her tells (hood,
sweater-paws) as silent punchlines.

#TODO(agent): pick a run of ~4 strips for the first post batch (dialogue locked:
short EN; layout locked: 2×2 grid).
