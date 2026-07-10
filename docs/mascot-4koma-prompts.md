# Noa Mascot — 4-koma Image-Generation Prompts

**One block = one complete 2×2 strip.** Each prompt below is fully self-contained:
copy a single block, paste it into your image generator, get the whole four-panel
strip with the short English dialogue baked in. Nothing to prepend or concatenate.
Scripts these render: `mascot-4koma.md`.

## Notes

- **Layout**: 2×2 grid, square 1:1, read **top-left → top-right → bottom-left →
  bottom-right**.
- **Text**: short EN lines are baked into the bubbles. They render best on
  text-capable models (Ideogram / gpt-image / nano-banana). If type comes out
  messy, drop the `bubble "…"` clauses and letter in post from `mascot-4koma.md`.
- **`Avoid:`** is the negative prompt inline. Midjourney users: move those terms to
  `--no`.
- **Consistency**: for print-quality finals, split a block into its four TL/TR/BL/BR
  lines and generate each with the same opening paragraph via i2i / a locked seed.

---

## Copy-paste prompts

### #1 — It Works On My Machine
```
manga 4-koma, 2x2 four-panel grid, square 1:1, four equal panels, clean gutters,
read top-left → top-right → bottom-left → bottom-right. Consistent characters in
every panel: Noa — a petite short girl, sleek jet-black bob with blunt bangs,
oversized black hoodie, one warm signature-orange (#E8A33D) accent; Ember — a small
round ghost with a faint warm-orange inner glow and a simple two-dot face. Clean
flat cel-shaded webcomic style, crisp black outlines, limited charcoal + warm-orange
palette, soft screentone, short clean English speech-bubble lettering.
(TL) Noa at her desk, flat deadpan, a wall screen behind her glowing red with a big
red ✗ — bubble "…passes here."
(TR) close on Ember squinting up at the red ✗, one nervous sweat drop — bubble
"it's SO red."
(BL) Noa gesturing flatly at her own all-green screen, unbothered — bubble "not my
bug."
(BR) Ember lifting a laptop overhead like an offering, Noa staring at it one beat
too long — bubble "…ship your laptop?"
Avoid: garbled text, extra fingers, deformed hands, realistic photo, cluttered
background, multiple accent colors, inconsistent character design, watermark.
```

### #2 — Rubber Duck
```
manga 4-koma, 2x2 four-panel grid, square 1:1, four equal panels, clean gutters,
read top-left → top-right → bottom-left → bottom-right. Consistent characters in
every panel: Noa — a petite short girl, sleek jet-black bob with blunt bangs,
oversized black hoodie, one warm signature-orange (#E8A33D) accent; Ember — a small
round ghost with a faint warm-orange inner glow and a simple two-dot face. Clean
flat cel-shaded webcomic style, crisp black outlines, limited charcoal + warm-orange
palette, soft screentone, short clean English speech-bubble lettering.
(TL) Noa turned to Ember, dead serious, one finger raised mid-explanation — bubble
"so the bug is—"
(TR) close on Ember sitting bolt upright, proud attentive nod, tiny sparkle of
purpose — bubble "mm!"
(BL) Noa's eyes widening a fraction, already standing and turning away in sudden
realization — bubble "—oh. never mind."
(BR) Ember alone in frame still nodding earnestly at empty air, an empty chair
beside it — bubble "…I helped?"
Avoid: garbled text, extra fingers, deformed hands, realistic photo, cluttered
background, multiple accent colors, inconsistent character design, watermark.
```

### #3 — The One-Character Fix
```
manga 4-koma, 2x2 four-panel grid, square 1:1, four equal panels, clean gutters,
read top-left → top-right → bottom-left → bottom-right. Consistent characters in
every panel: Noa — a petite short girl, sleek jet-black bob with blunt bangs,
oversized black hoodie, one warm signature-orange (#E8A33D) accent; Ember — a small
round ghost with a faint warm-orange inner glow and a simple two-dot face. Clean
flat cel-shaded webcomic style, crisp black outlines, limited charcoal + warm-orange
palette, soft screentone, short clean English speech-bubble lettering.
(TL) Noa buried in a wall of glowing code tabs at 3 AM, messy hair, exhausted,
sleeves over hands as sweater-paws — caption box "hour 3."
(TR) Ember face-down asleep on the desk, a tiny snore bubble — no dialogue.
(BL) extreme close-up on one line of code, a single "=" turning into "==" highlighted
in warm orange — bubble "…"
(BR) Noa perfectly flat and deadpan in foreground, Ember behind her jolting awake
mid-scream with motion lines — bubble "THREE HOURS?!"
Avoid: garbled text, extra fingers, deformed hands, realistic photo, cluttered
background, multiple accent colors, inconsistent character design, watermark.
```

### #4 — The Five-Minute Estimate
```
manga 4-koma, 2x2 four-panel grid, square 1:1, four equal panels, clean gutters,
read top-left → top-right → bottom-left → bottom-right, IDENTICAL camera and pose
in every panel — only the window light / time of day changes. Consistent characters:
Noa — a petite short girl, sleek jet-black bob with blunt bangs, oversized black
hoodie, one warm signature-orange (#E8A33D) accent; Ember — a small round ghost with
a faint warm-orange inner glow and a simple two-dot face. Clean flat cel-shaded
webcomic style, crisp black outlines, limited charcoal + warm-orange palette, soft
screentone, short clean English speech-bubble lettering.
(TL) Noa hands in hoodie pocket glancing at a ticket, window behind her in bright
daylight — bubble "five minutes."
(TR) same framing and pose, window now sunset orange — no dialogue.
(BL) same framing, window full night, Ember asleep, a small pile of mugs — no
dialogue.
(BR) same framing, window sunrise, Noa still typing utterly unbothered with faint
dark circles — bubble "…almost done."
Avoid: garbled text, extra fingers, deformed hands, realistic photo, cluttered
background, multiple accent colors, inconsistent character design, watermark.
```

### #5 — git blame
```
manga 4-koma, 2x2 four-panel grid, square 1:1, four equal panels, clean gutters,
read top-left → top-right → bottom-left → bottom-right. Consistent characters in
every panel: Noa — a petite short girl, sleek jet-black bob with blunt bangs,
oversized black hoodie, one warm signature-orange (#E8A33D) accent; Ember — a small
round ghost with a faint warm-orange inner glow and a simple two-dot face. Clean
flat cel-shaded webcomic style, crisp black outlines, limited charcoal + warm-orange
palette, soft screentone, short clean English speech-bubble lettering.
(TL) Noa reading her screen with quiet disgust, slight frown — bubble "…who wrote
this."
(TR) Ember helpfully tapping a key, small sparkles of initiative — bubble "git
blame!"
(BL) screen close-up, an author label reading "Noa" with an old date, spotlighted
in warm orange — no dialogue.
(BR) Ember pointing at the screen, Noa yanking her hood halfway up to hide with
averted eyes and a faint blush — bubble "…nobody."
Avoid: garbled text, extra fingers, deformed hands, realistic photo, cluttered
background, multiple accent colors, inconsistent character design, watermark.
```

### #6 — --force
```
manga 4-koma, 2x2 four-panel grid, square 1:1, four equal panels, clean gutters,
read top-left → top-right → bottom-left → bottom-right. Consistent characters in
every panel: Noa — a petite short girl, sleek jet-black bob with blunt bangs,
oversized black hoodie, one warm signature-orange (#E8A33D) accent; Ember — a small
round ghost with a faint warm-orange inner glow and a simple two-dot face. Clean
flat cel-shaded webcomic style, crisp black outlines, limited charcoal + warm-orange
palette, soft screentone, short clean English speech-bubble lettering.
(TL) close on Noa's hand, cursor hovering over a "force push" button, one finger
raised, her face flat and calm — bubble "…it's fine."
(TR) the screen flashing white, a beat of dead silence, Noa expressionless — no
dialogue.
(BL) Ember spiraling into pure horror, wide eyes, tiny hands to its face — bubble
"our HISTORY—!!"
(BR) Noa already calmly typing again and serene in foreground, Ember collapsing in
relief behind her with a small warm-orange safe glow — bubble "…reflog."
Avoid: garbled text, extra fingers, deformed hands, realistic photo, cluttered
background, multiple accent colors, inconsistent character design, watermark.
```

### #7 — Just One More Feature
```
manga 4-koma, 2x2 four-panel grid, square 1:1, four equal panels, clean gutters,
read top-left → top-right → bottom-left → bottom-right. Consistent characters in
every panel: Noa — a petite short girl, sleek jet-black bob with blunt bangs,
oversized black hoodie, one warm signature-orange (#E8A33D) accent; Ember — a small
round ghost with a faint warm-orange inner glow and a simple two-dot face. Clean
flat cel-shaded webcomic style, crisp black outlines, limited charcoal + warm-orange
palette, soft screentone, short clean English speech-bubble lettering.
(TL) cozy dark room, warm-orange screen glow lighting Noa's calm-determined face —
bubble "one more, then bed."
(TR) Ember yawning and curling up asleep against the keyboard — no dialogue.
(BL) a spinning clock and a growing pile of mugs, time-lapse feel, Noa still lit by
the screen — no dialogue.
(BR) sunrise light through the window, Ember waking to find Noa in the exact same
pose, Ember's silent dread — bubble "…one more."
Avoid: garbled text, extra fingers, deformed hands, realistic photo, cluttered
background, multiple accent colors, inconsistent character design, watermark.
```

### #8 — The Heisenbug
```
manga 4-koma, 2x2 four-panel grid, square 1:1, four equal panels, clean gutters,
read top-left → top-right → bottom-left → bottom-right. Consistent characters in
every panel: Noa — a petite short girl, sleek jet-black bob with blunt bangs,
oversized black hoodie, one warm signature-orange (#E8A33D) accent; Ember — a small
round ghost with a faint warm-orange inner glow and a simple two-dot face. Clean
flat cel-shaded webcomic style, crisp black outlines, limited charcoal + warm-orange
palette, soft screentone, short clean English speech-bubble lettering.
(TL) Noa glaring at a small glitchy shadow-bug creature on her screen, challenging
stare — bubble "reproduce."
(TR) Noa adds one glowing line of code and the bug instantly poofs into nothing,
Noa mildly satisfied — bubble "…gone."
(BL) Noa deletes the line and the bug pops back looking smug with tiny crossed arms
— no dialogue.
(BR) Noa's long flat stare, the bug ducking behind Ember, Ember shrugging helplessly
— bubble "…"
Avoid: garbled text, extra fingers, deformed hands, realistic photo, cluttered
background, multiple accent colors, inconsistent character design, watermark.
```

#TODO(agent): after a test render, tune the shared opening paragraph (screentone
amount, outline weight) to the chosen generator, then propagate the locked house
style across all eight blocks.
