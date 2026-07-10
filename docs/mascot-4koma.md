# Noa Mascot — Engineer 4-koma Scripts

Recurring 4-panel strips: **engineer "あるある" (relatable dev moments)** starring
Noa and her companion Ember. The engine is the **gap**: Noa stays flat and
unbothered while Ember carries all the emotion — the punchline lands in the space
between her calm and the disaster.

## Format & voice

- **4 panels**, beat structure: **setup → build → turn → punchline** (panel 4).
- **Noa**: short flat lines, trailing "…", never panics. Her tells: hood halfway up
  = embarrassed; sweater-paws = tired (see `mascot-ip-bible.md` §1).
- **Ember**: the reaction engine — screams, panics, cheers, the feelings Noa won't
  show. Food-motivated, clingy, glows with emotion.
- **Art**: each panel = `[IP Common Block]` (`mascot-ip.md`) + the panel's `[visual]`.
- Keep dialogue sparse; let the faces and the beat do the work.

---

## Strips

### #1 — It Works On My Machine
*あるある: the CI is red, your screen is green, and both are "true".*
1. `[Noa at her desk, flat face; a CI dashboard glowing red behind her]` Noa: "…it passes here."
2. `[Ember squinting at the red ✗, sweating a little]` Ember: "it's, um. very red though."
3. `[Noa gestures at her own screen — all green checks]` Noa: "not my problem."
4. `[punchline — Ember lifts the whole laptop up like an offering]` Ember: "…so we ship your machine?" — Noa, considering it one beat too long: "…"

### #2 — Rubber Duck
*あるある: you solve it the instant you explain it out loud.*
1. `[Noa turns to Ember, dead serious]` Noa: "okay. the bug is—"
2. `[Ember sits bolt upright, proud to finally be useful, nodding]`
3. `[mid-sentence Noa's eyes widen a fraction; she stands]` Noa: "—oh. it's the cache. never mind."
4. `[punchline — Ember alone, still nodding earnestly at empty air]` Ember: "…I helped. right?"

### #3 — The One-Character Fix
*あるある: three hours of debugging, and the fix was `=` → `==`.*
1. `[Noa buried in 40 open tabs, a 3:00 AM clock, hair a mess]` caption: "hour three."
2. `[Ember face-down asleep on the desk, tiny snore]`
3. `[extreme zoom on one line of code: a single `=` becoming `==`]` Noa: "…"
4. `[punchline — Noa perfectly flat; Ember jolts awake screaming the rage Noa refuses to feel]` Noa: "." — Ember: "THREE?! HOURS?!"

### #4 — The Five-Minute Estimate
*あるある: "five minutes" is a unit of hope, not time.*
1. `[Noa, hands in hoodie pocket, glancing at a ticket]` Noa: "five minutes."
2. `[same desk, window behind her: sunset]`
3. `[window: full night; Ember asleep, mugs accumulating]`
4. `[punchline — window: sunrise; Noa still typing, utterly unbothered]` Noa: "…almost done."

### #5 — git blame
*あるある: "who wrote this garbage" → it was you, six months ago.*
1. `[Noa reading code with quiet disgust]` Noa: "…who wrote this."
2. `[Ember helpfully runs `git blame`, sparkles of initiative]`
3. `[the screen: author — "Noa", 6 months ago]`
4. `[punchline — Ember points at the screen; Noa yanks her hood halfway up (embarrassed tell)]` Ember: "it says No—" — Noa, hood up: "…didn't."

### #6 — --force
*あるある: the calm before `git push --force`, and the abyss after.*
1. `[Noa's cursor hovering over `git push --force`, one finger raised]` Noa: "…it's fine."
2. `[she commits to it; the screen flashes; a full beat of dead silence]`
3. `[Ember doing the math, spiraling into pure horror]` Ember: "the whole team's history—!!"
4. `[punchline — Noa already typing again, serene]` Noa: "…reflog." — `[Ember collapses in relief]`

### #7 — Just One More Feature
*あるある: "one more thing before bed" is a promise to the sunrise.*
1. `[cozy dark room, warm-orange screen glow]` Noa: "one more feature, then bed."
2. `[Ember yawns, curls up asleep against the keyboard]`
3. `[a clock spinning; the mug pile grows]`
4. `[punchline — sunrise; Ember wakes to find Noa in the exact same spot]` Noa: "…one more." — Ember: `[silent, dawning dread]`

### #8 — The Heisenbug
*あるある: the bug vanishes the moment you add a print statement.*
1. `[Noa glaring at a glitchy shadow-bug on screen]` Noa: "reproduce. now."
2. `[she adds a `print(…)` line; the bug instantly vanishes]` Noa: "…gone."
3. `[she deletes the print; the bug pops back, smug]`
4. `[punchline — Noa's flat stare; the bug ducks behind Ember, who shrugs helplessly]` Noa: "…"

---

## Extending the series (more あるある to script)

Naming things (`data` → `data2` → `dataFinalReal`) · "no blockers" at standup (has
many) · the quick refactor that touched 40 files · TODO: fix later (never) · works
in dev, dies in prod · the solution arriving in the shower · regex → now two
problems · off-by-one · deleting `node_modules` as therapy · the Friday 5 PM
incident.

**Punchline engine (reuse every time)**: Noa stays flat → Ember overreacts → the
last panel is the gap between her calm and the size of the disaster. Ember gets no
credit for the wins and all the panic for the losses. Keep her tells (hood, sweater-
paws) as silent punchlines.

#TODO(agent): pick a run of ~4 strips for the first post batch; decide EN-only vs
JP-localized dialogue for the target platform (note/Zenn/X vs LINE).
