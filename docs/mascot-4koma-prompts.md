# Noa Mascot — 4-koma Image-Generation Prompts

Ready-to-generate prompts for the strips in `mascot-4koma.md`. Each panel =
**Panel Base** (character + style + framing DNA, prepended to every panel) **+**
the panel's clause below. Dialogue is **not** baked into the image — generate art
with empty bubbles and typeset the lines from `mascot-4koma.md` in post (diffusion
models garble text).

## Panel Base (prepend to every panel)

```
manga 4-koma panel, one consistent character Noa — a petite short girl, sleek
jet-black bob with blunt bangs, oversized black hoodie, a single warm
signature-orange (#E8A33D) accent; her companion Ember — a small round ghost with
a faint warm-orange inner glow and a simple two-dot face; clean flat cel-shaded
webcomic style, crisp black outlines, limited charcoal + warm-orange palette, soft
screentone shading, empty speech bubbles with NO text and NO lettering, strong
character consistency
```

## Negative Prompt (shared)

```
readable text, garbled letters, watermark, signature, extra fingers, deformed
hands, realistic photo, cluttered background, multiple accent colors, inconsistent
character design, tall model proportions, lowres, blurry, jpeg artifacts
```

## Workflow

- **Consistency**: lock Noa + Ember once (seed or a character reference from the
  Signature stand, `mascot-ip.md`), then generate every panel via i2i / same seed
  so the pair stays on-model across all four.
- **Framing**: keep the camera identical within a "time-lapse" strip (#4, #7) — the
  unchanging frame *is* the joke. Vary shots (wide / medium / close-up) in the
  reaction strips, and give panel 4 the tightest, most readable framing.
- **Aspect**: per-panel ≈ 1:1 (or 4:3). Full strip = 4 panels stacked, ≈ 9:16 → 1:4.
- **Text**: leave bubbles empty; letter the dialogue afterward from the script doc.

## Assembled example (Strip #1, Panel 1 — full copy-paste form)

```
manga 4-koma panel, one consistent character Noa — a petite short girl, sleek
jet-black bob with blunt bangs, oversized black hoodie, a single warm
signature-orange (#E8A33D) accent; her companion Ember — a small round ghost with
a faint warm-orange inner glow and a simple two-dot face; clean flat cel-shaded
webcomic style, crisp black outlines, limited charcoal + warm-orange palette, soft
screentone shading, empty speech bubbles with NO text and NO lettering, strong
character consistency + medium shot, Noa at her desk with a flat deadpan face, a
large wall screen behind her glowing red with a big ✗, a small green screen in
front of her, empty speech bubble over her head
```

> Every clause below concatenates the same way: `Panel Base + <clause>`.

---

## Panel clauses

### #1 — It Works On My Machine
- **P1**: `+ medium shot, Noa at her desk with a flat deadpan face, a large wall screen behind her glowing red with a big ✗, a small green screen in front of her, empty bubble`
- **P2**: `+ close-up on Ember squinting up at the red ✗, one nervous sweat drop, timid pose, empty bubble`
- **P3**: `+ Noa gesturing flatly at her own all-green screen, unbothered, half-lidded eyes, empty bubble`
- **P4**: `+ punchline wide shot, Ember lifting a laptop overhead like an offering toward the red screen, Noa staring at it one beat too long as if actually considering it, comedic pause, empty bubble`

### #2 — Rubber Duck
- **P1**: `+ two-shot, Noa turned to Ember dead-serious, one finger raised mid-explanation, empty bubble`
- **P2**: `+ close-up, Ember sitting bolt upright, proud attentive nod, tiny sparkle of purpose`
- **P3**: `+ Noa mid-gesture with eyes widening a fraction, already standing and turning away in sudden realization, empty bubble`
- **P4**: `+ punchline, Ember alone in frame still nodding earnestly at empty air, an empty chair beside it, faint confusion, empty bubble`

### #3 — The One-Character Fix
- **P1**: `+ wide shot, Noa buried in a wall of glowing code tabs at 3 AM, messy hair, exhausted, sleeves over hands as sweater-paws`
- **P2**: `+ close-up, Ember face-down asleep on the desk, a tiny snore bubble`
- **P3**: `+ extreme close-up on a single line of code, one "=" turning into "==" highlighted in warm orange, Noa's flat eyes faintly reflected`
- **P4**: `+ punchline split composition, Noa perfectly flat and deadpan in foreground, Ember behind her jolting awake mid-scream with motion lines, comedic contrast, empty bubbles`

### #4 — The Five-Minute Estimate
*(identical camera every panel — only the window light / time changes)*
- **P1**: `+ medium shot, Noa hands in hoodie pocket glancing at a ticket, casual, window behind her in bright daylight, empty bubble`
- **P2**: `+ same framing and pose, window now sunset orange`
- **P3**: `+ same framing, window full night, Ember asleep, a small pile of mugs`
- **P4**: `+ punchline, same framing, window sunrise, Noa still typing utterly unbothered with faint dark circles, empty bubble`

### #5 — git blame
- **P1**: `+ close-up, Noa reading her screen with quiet disgust, slight frown, empty bubble`
- **P2**: `+ Ember helpfully tapping a key, small sparkles of initiative`
- **P3**: `+ screen close-up, an author label reading "Noa" with an old date, spotlighted in warm orange`
- **P4**: `+ punchline, Ember pointing at the screen, Noa yanking her hood halfway up to hide with averted eyes and a faint blush (embarrassed tell), empty bubbles`

### #6 — --force
- **P1**: `+ close-up on Noa's hand, cursor hovering over a "force push" button, one finger raised, her face flat and calm, empty bubble`
- **P2**: `+ the screen flashing white, a beat of dead silence, Noa expressionless`
- **P3**: `+ Ember spiraling into pure horror, wide eyes, tiny hands to its face, empty bubble`
- **P4**: `+ punchline, Noa already calmly typing again and serene in foreground, Ember collapsing in relief behind her with a small warm-orange "safe" glow`

### #7 — Just One More Feature
- **P1**: `+ cozy dark room, warm-orange screen glow lighting Noa's calm-determined face, empty bubble`
- **P2**: `+ Ember yawning and curling up asleep against the keyboard`
- **P3**: `+ a spinning clock and a growing pile of mugs, time-lapse feel, Noa still lit by the screen`
- **P4**: `+ punchline, sunrise light through the window, Ember waking to find Noa in the exact same pose, Ember's silent dread, empty bubble`

### #8 — The Heisenbug
- **P1**: `+ Noa glaring at a small glitchy shadow-bug creature on her screen, challenging stare, empty bubble`
- **P2**: `+ Noa adds one glowing line of code and the bug instantly poofs into nothing, Noa mildly satisfied, empty bubble`
- **P3**: `+ Noa deletes the line and the bug pops back looking smug with tiny crossed arms`
- **P4**: `+ punchline, Noa's long flat stare, the bug ducking behind Ember, Ember shrugging helplessly, empty bubbles`

---

## Single-image 4-koma grid (fast path, any strip)

Wrap a whole strip in one generation instead of four:

```
[Panel Base] + a vertical 4-panel manga 4-koma strip, four equal stacked panels
read top to bottom with clean gutters, telling: (1) <P1 clause> (2) <P2 clause>
(3) <P3 clause> (4) <P4 clause>, consistent character across all panels, empty
speech bubbles with no text, 9:16
```

> Faster but less controllable; per-panel + i2i keeps the pair more on-model. Use
> the grid for drafts, per-panel for finals.

#TODO(agent): after a test render, tune the Panel Base tokens (screentone amount,
outline weight) to the chosen generator, then lock a house style string.
