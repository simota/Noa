# Noa Mascot — 4-koma Image-Generation Prompts

**One block = one complete 2×2 strip.** Each prompt below is fully self-contained:
copy a single block, paste it into your image generator, get the whole four-panel
strip with the short English dialogue baked in. Nothing to prepend or concatenate.
Scripts these render: `mascot-4koma.md`.

## Notes

- **Layout**: 2×2 grid, square 1:1, read **top-left → top-right → bottom-left →
  bottom-right**.
- **Clarity**: panel 1 sets the situation, the bottom-right bubble **states the
  payoff** — so the joke reads without the viewer already knowing the meme.
- **Text**: short EN lines are baked into the bubbles. They render best on
  text-capable models (Ideogram / gpt-image / nano-banana). If type comes out
  messy, drop the `bubble "…"` clauses and letter in post from `mascot-4koma.md`.
- **`Avoid:`** is the negative prompt inline. Midjourney users: move those terms to
  `--no`.
- **Consistency**: for print finals, split a block into its four TL/TR/BL/BR lines
  and generate each with the same opening paragraph via i2i / a locked seed.

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
(TL) Noa's laptop showing all-green passing tests, she looks calm — bubble "all
green here."
(TR) Ember pointing in alarm at a big CI wall-screen that is all red with an ✗ —
bubble "CI is ALL red…!"
(BL) Noa flat with a small shrug — bubble "works on my machine."
(BR) Ember shoving Noa's laptop into a cardboard shipping box — bubble "…then we
ship your machine?!"
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
(TL) Noa turning to Ember, serious, one finger raised — bubble "let me explain the
bug to you."
(TR) Ember puffing up proudly, listening like a good rubber duck — bubble "mm-hm!"
(BL) Noa lighting up mid-sentence, already turning away to her keyboard — bubble
"…oh. fixed it. thanks!"
(BR) Ember alone with a deadpan look, an empty chair beside it — bubble "…I said
nothing."
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
(TL) Noa wrecked and hollow-eyed, a clock reading 3:00 behind her — bubble "found
the bug."
(TR) extreme close-up on a line of code with one tiny missing semicolon glowing
orange — no dialogue.
(BL) Noa perfectly flat — bubble "…one character."
(BR) Ember erupting with both arms up — bubble "THREE HOURS FOR ONE CHARACTER?!"
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
daylight — bubble "give me five minutes."
(TR) same framing and pose, window now sunset orange — no dialogue.
(BL) same framing, window full night, Ember asleep, a small pile of mugs — no
dialogue.
(BR) same framing, window sunrise, Noa still typing with faint dark circles — bubble
"…five more minutes."
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
(TL) Noa scowling at code in disgust — bubble "who wrote this garbage?"
(TR) Ember eagerly running the command with sparkles — bubble "git blame~!"
(BL) a screen, big and legible, reading "Author: Noa — 6 months ago" — no dialogue.
(BR) Noa yanking her hood down over her face to hide, faint blush — bubble "…forget
I asked."
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
(TL) Noa's finger over the Enter key running a "git push --force" command, calm face
— bubble "it's fine."
(TR) the screen flashing as the team's commit list gets wiped to empty, dead silence,
Noa expressionless — no dialogue.
(BL) Ember melting down in horror, hands to its face — bubble "you deleted EVERYONE'S
work!!"
(BR) Noa calmly typing as the commits restore on screen — bubble "git never forgets."
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
(TL) cozy dark room, warm-orange screen glow on Noa's calm-determined face — bubble
"one more feature, then bed."
(TR) Ember yawning and curling up asleep against the keyboard — no dialogue.
(BL) a spinning clock and a growing pile of mugs, time-lapse feel, Noa still lit by
the screen — no dialogue.
(BR) sunrise light through the window, Ember waking to find Noa in the exact same
pose, Ember's silent dread — bubble "…one more feature."
Avoid: garbled text, extra fingers, deformed hands, realistic photo, cluttered
background, multiple accent colors, inconsistent character design, watermark.
```

### #8 — The Heisenbug (the log that can't be removed)
```
manga 4-koma, 2x2 four-panel grid, square 1:1, four equal panels, clean gutters,
read top-left → top-right → bottom-left → bottom-right. Consistent characters in
every panel: Noa — a petite short girl, sleek jet-black bob with blunt bangs,
oversized black hoodie, one warm signature-orange (#E8A33D) accent; Ember — a small
round ghost with a faint warm-orange inner glow and a simple two-dot face. Clean
flat cel-shaded webcomic style, crisp black outlines, limited charcoal + warm-orange
palette, soft screentone, short clean English speech-bubble lettering.
(TL) Noa glaring at a glitchy crash error on her screen — bubble "why does it crash?"
(TR) Noa adds one glowing "print()" line and the glitch instantly vanishes, mildly
surprised — bubble "add a log… it stops?"
(BL) Noa deletes the "print()" line and the glitchy crash pops back, looking smug —
bubble "remove it… it's back."
(BR) Noa leaves the log in forever with a flat resigned face, the screen now stable —
bubble "…the log stays."
Avoid: garbled text, extra fingers, deformed hands, realistic photo, cluttered
background, multiple accent colors, inconsistent character design, watermark.
```

#TODO(agent): after a test render, tune the shared opening paragraph (screentone
amount, outline weight) to the chosen generator, then propagate the locked house
style across all eight blocks.
