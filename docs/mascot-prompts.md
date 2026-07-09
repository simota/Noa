# Noa Mascot Image-Generation Prompts

Anthropomorphized mascot for Noa (a Rust clone of the Ghostty terminal). Prompts for AI image generation.

## Character Core

- Black bob cut, blunt bangs, short stature (petite, youthful build)
- Sharp, slightly narrow eyes = strong will; gaze looks straight into the camera, never fawning
- Introverted vibe: deadpan-to-flat expression, few words, oversized black hoodie, slight slouch
- Yet strong-willed with her own values: rebuilds an existing product (Ghostty) through her own interpretation — her own path
- Terminal motif: dark base + a single Rust/terminal-orange accent, block cursor, a subtle nod to Ghostty's "ghost"

---

## Common Block (prepend to every prompt)

```
full-body mascot illustration of a petite short girl, sleek jet-black bob cut
with blunt bangs, oversized black hoodie with a small terminal-cursor emblem
on the chest, flat vector / clean cel-shaded style, dark charcoal background,
monochrome palette with a single warm terminal-orange (#E8A33D) accent,
a tiny ghost sprite floating beside her, sticker-ready, crisp outlines, 4k
```

## Negative Prompt (shared)

```
tall, realistic photo, cluttered background, extra fingers, deformed hands,
lowres, blurry, watermark, jpeg artifacts, multiple accent colors
```

---

## Expression Variants (four base emotions + 6 extras)

Each prompt concatenates the variant text after `[Common Block]`.

### Neutral (base standing pose / icon reference)

```
[Common Block] + calm deadpan expression, straight mouth, direct steady gaze
at the viewer, relaxed neutral standing pose, hands at sides, the ghost sprite
hovering quietly, the cursor emblem in a steady soft-orange blink
```

> Treat this as the canonical reference so the other expressions stay consistent.

### Joy

```
[Common Block] + soft genuine half-smile, eyes gently curved, faint blush,
a small confident thumbs-up, relaxed shoulders, the ghost sprite bouncing
happily, the block-cursor emblem glowing bright orange, tiny sparkle accents
```

> She's introverted, so a restrained-but-genuine smile — not a beaming grin.

### Anger

```
[Common Block] + sharp furrowed brows, sideways glare, mouth in a tight frown,
arms crossed firmly, feet planted, standing her ground (strong-willed),
the ghost sprite puffed up with an angry pop mark, the cursor emblem
flashing an intense orange, subtle heat-shimmer lines
```

> Her values won't budge — anger as "pushing through," not emotional flailing.

### Sadness

```
[Common Block] + downcast eyes, slightly slouched posture, hood partially up,
one hand tugging the drawstring, quiet melancholic expression, a single small
glisten at the eye, the ghost sprite drooping low, the cursor emblem dimmed
to a faint slow-blinking orange, cool muted lighting
```

> Pulling the hood partway up is her withdrawn, downcast tell.

### Ease / Fun

```
[Common Block] + calm content expression, faint smirk, hands in hoodie pocket,
casual relaxed lean, earbuds in one ear, the ghost sprite lounging on her
shoulder, the cursor emblem in a steady warm-orange glow, cozy chill mood
```

> Interpreted as relaxed / at-her-own-pace (the type who's comfortable alone).

### Embarrassed

```
[Common Block] + strong blush across cheeks, eyes darting away, mouth in a
small awkward line, one hand pulling the hood halfway up to hide, tense shy
posture, the ghost sprite peeking from behind her, the cursor emblem flushing
a warm pink-orange
```

### Smug

```
[Common Block] + smug confident smirk, half-lidded eyes, chin slightly raised,
one hand on hip / index finger pointing up as if making a point, proud posture,
the ghost sprite mimicking the pose, the cursor emblem pulsing bright orange,
tiny "✓" spark accent
```

> Her face when she explains her own values — a "my interpretation is right" look.

### Surprised

```
[Common Block] + wide startled eyes, small open mouth, slight lean-back,
hands raised near chest, hood slipping off, the ghost sprite popping up
with a spark, the cursor emblem freezing mid-blink in bright orange
```

### Sleepy / Drowsy

```
[Common Block] + half-closed heavy eyes, tiny yawn, slouched low posture,
oversized hoodie sleeves covering hands (sweater paws), the ghost sprite
dozing on her head, the cursor emblem in a slow lazy fade-blink
```

> Late-night terminal-work energy. Pairs well with the introverted + petite build.

### Focused / Working (at the terminal — good for key visuals)

```
[Common Block] + intense focused stare, faint monospace code glow reflected
on her face, fingers poised as if typing, leaning slightly forward, the ghost
sprite watching over her shoulder, the cursor emblem streaming fast orange
blinks, subtle dark-UI light from below
```

> Noa's "day job" scene. Good for a key visual / OGP image.

---

## Scenes

Like the expression variants, concatenate each prompt after `[Common Block]`.

### Product-tied (key visual / OGP / README header)

**Boot splash — homage to Ghostty**

```
[Common Block] + standing proud in front of a giant glowing terminal window,
looking up at flowing orange monospace code raining down like Matrix, one hand
reaching toward it, the ghost sprite trailing a light streak, hero composition,
dramatic backlight
```

> The "rebuilding Ghostty with her own hands" narrative. Great for the README top.

**Multi-pane / tab avatar**

```
[Common Block] + surrounded by several floating semi-transparent terminal
panes arranged around her like screens, calmly orchestrating them, small ghost
sprites each sitting on a pane, focused confident look, dark UI glassmorphism
```

> Visualizes tabs / sidebar / split panes as a character. For a feature diagram.

**Rust crates as companions (group shot)**

```
[Common Block] + chibi group shot, the main girl center, surrounded by tiny
mascot sprites each labeled with a monospace tag (vt, grid, font, pty, render),
team pose, dark background, single orange accent, sticker sheet layout
```

> Turns `noa-vt` / `noa-grid`… into mini characters. Fun as a settei sheet.

### Daily life / mood (SNS icons, stickers)

**Late-night coding**

```
[Common Block] + sitting cross-legged on a chair in a dark room lit only by
the orange terminal glow, hoodie up, sipping from a mug, cozy lonely-but-content
vibe, ghost sprite curled up asleep nearby, warm rim light
```

**Mug / coffee break**

```
[Common Block] + holding an oversized mug with the terminal-cursor logo,
tiny satisfied smirk, steam rising, the ghost sprite floating over the mug,
chill half-body shot
```

**Cat and terminal**

```
[Common Block] + a small black cat sitting on her keyboard blocking the screen,
her deadpan unamused face, the ghost sprite shrugging, cozy desk clutter, warm
low light, relatable dev-life humor
```

### Brand / logo-leaning

**Minimal icon reduction**

```
petite black-bob girl reduced to a clean minimal logo mark, just the silhouette
with blunt bangs and a single orange block-cursor as the eye, flat 2-color
(black + #E8A33D), app-icon ready, centered, no background detail
```

> Abstracted version, easy to reuse as an app icon / favicon.

**Wide banner composition**

```
[Common Block] but wide banner composition, girl on the left third looking
right toward large "Noa" monospace wordmark, ghost sprite dotting the letters,
orange accent underline like a blinking prompt, GitHub social-preview ratio
```

> **Use-case summary**
> - README / OGP → boot splash, wide banner
> - Settei sheet → crate group shot, three-view sheet
> - SNS / stickers → late-night coding, mug, cat

### Rest / sleep

**Asleep at the keyboard — passed out from exhaustion**

```
[Common Block] + fast asleep face-down on the desk, cheek resting on the
keyboard, arms sprawled, drool-tiny bubble, monospace keys imprinted faintly
on her cheek, the ghost sprite gently draping a tiny blanket over her, the
cursor emblem in a slow sleepy fade-blink, warm dim orange desk light, quiet
late-night hush
```

> The classic burn-out slump. Late-night "ran out of fuel" energy.

**Curled up in the chair, hood up**

```
[Common Block] + curled up asleep in an oversized office chair, knees pulled
in, hood up covering half her face, oversized sleeves as sweater paws, peaceful
worn-out expression, the ghost sprite curled up sleeping on her lap, a closed
laptop glowing faint orange nearby, cozy exhausted stillness
```

**Micro-nap mid-task (still sitting)**

```
[Common Block] + sitting upright but dozed off mid-work, head tilted, eyes
closed, a single "Zzz" in monospace floating up, one hand still loosely on the
keyboard, empty mug beside her, the ghost sprite yawning and drifting off too,
soft orange screen glow on her sleeping face
```

> The dozing-off version — half mid-task, drifted under.

**Blanket burrito on the couch**

```
[Common Block] + wrapped up like a burrito in a dark blanket on a small couch,
only her sleeping face and messy black bob peeking out, deeply worn-out but
content, the ghost sprite nestled against her, a paused terminal on a screen in
the background, tranquil overnight scene
```

---

## Operational Notes

- **Consistency tip**: Lock the Neutral pose first with a fixed seed, then run every other variant via i2i (image-to-image) so the art style matches.
- **Set production**: A one-image expression pack (3×3 grid) prompt is also possible.
- **Color**: Restrict the accent to the single terminal-orange `#E8A33D` (multiple accents scatter the look).

## Reference: Portrait Version (single illustration, not a mascot)

```
anime-style portrait of a petite short girl with a sleek black bob haircut,
straight-cut bangs, sharp calm almond eyes with a quiet strong-willed gaze
looking directly at the viewer, subtle flat expression (introverted, reserved),
wearing an oversized dark charcoal hoodie, slightly slouched posture but
confident presence, one accent of warm terminal-orange (#E8A33D) on her
hoodie drawstrings or hairclip, dark UI aesthetic, faint monospace glyphs and
a blinking block-cursor motif glowing softly in the background, minimal
ghost-like wisp near her shoulder as a hidden motif, moody low-key lighting,
muted dark palette with a single orange highlight, clean cel-shaded rendering,
detailed eyes, high quality, 4k
```
