# Noa Mascot Image-Generation Prompts

Anthropomorphized mascot for Noa (a Rust clone of the Ghostty terminal). Prompts for AI image generation.

## Character Core

> Full canonical profile: **`mascot-profile.md`**. Below is the prompt-tuning digest.

- Jet-black bob, full blunt bangs; **petite, youthful ~5.5 heads tall** (balanced, slightly smaller head)
- **Large, dark deadpan eyes** with small catchlights and a quiet, strong-willed, unfawning gaze; a small flat mouth
- Introverted, few words, **cool and self-possessed at her own pace** (strong-willed underneath, never fawning); **very oversized black hoodie worn dress-like** (hem at mid-thigh)
- Chest emblem = **orange shell-prompt `>▮`**; **black crew socks** + bare legs; chunky **black sneakers with orange accents** (same `>▮` on the side)
- Strong-willed with her own values: rebuilds an existing product (Ghostty) through her own interpretation — her own path
- Terminal motif: dark base + a single terminal-orange accent, block cursor, a subtle nod to Ghostty's "ghost"; presented with a white die-cut sticker outline

---

## Common Block (prepend to every prompt)

```
full-body mascot illustration of a petite, youthful girl about 5.5 heads tall
(balanced proportions, a slightly smaller head), sleek jet-black bob with full
blunt bangs, large dark deadpan eyes with small catchlights and a quiet,
strong-willed, unfawning gaze, and a small flat mouth, a cool and self-possessed
at-her-own-pace air (introverted yet strong-willed, unbothered and quietly confident
— deadpan on the surface, soft underneath), a very oversized black hoodie
worn dress-like
(hem at mid-thigh) with an orange shell-prompt ">|" (a chevron plus a block cursor)
emblem on the chest, black crew socks with bare legs, chunky black dad-sneakers
with orange accents, flat vector / clean cel-shaded style, thick clean outlines,
white die-cut sticker border, dark charcoal background, monochrome palette with a
single warm terminal-orange (#E8A33D) accent, a tiny pure-white ghost sprite
(Ember: a chubby round white ghost with a wavy hem and one flicked tail, two big
round black eyes with tiny white catchlights, a small oval mouth and tiny rosy cheek
blushes, soft and cute) floating beside her, sticker-ready, 4k
```

> **Attitude to convey** (bake into every prompt): cool, self-possessed, and at her
> own pace — introverted but quietly strong-willed and unbothered, never fawning or
> perky. Deadpan on the surface, soft underneath. Let **Ember carry the emotion**
> beside her; the flat-girl / expressive-ghost contrast is what sells her character.

## What to avoid (negative guidance)

**Primary generator = GPT-Image-2** (OpenAI's ChatGPT image generator). It follows
instructions and has **no separate negative-prompt field** — fold the exclusions
below into the prompt itself as plain "do not / avoid …" sentences. The token block
after it is for **Stable Diffusion / Midjourney**, which do take a negative field
(Midjourney: `--no ...`).

### Avoid — phrase as instructions (GPT-Image-2)

- Keep her **petite, ~5.5 heads tall** (balanced) — not a tall / adult / mature woman, no long
  realistic legs, no elongated body.
- Eyes stay **large but flat and deadpan, with a quiet strong-willed gaze** — not
  sparkly wide moe eyes, no heavy eyelashes, no winking, no glossy over-highlighting
  (a single small catchlight is fine).
- Exactly **one orange (`#E8A33D`) accent** — no second accent colour, no rainbow;
  the hoodie stays **plain black** (no patterns or all-over graphics).
- The chest emblem is the **orange `>|` shell-prompt** — no other logos, brand
  marks, letters, or words anywhere, and never drop the emblem.
- **Ember is pure white** — never orange / tinted / glowing-orange; keep it a simple
  plump ghost (two oval eyes + a small oval mouth), not scary, not a sheet-ghost,
  not big-eyed, no hands, and **only one ghost**.
- Footwear = **black crew socks + black sneakers with orange accents** — not
  barefoot, no boots, heels, or white shoes.
- Hold the **flat cel-shaded** look with the **white die-cut sticker outline** on a
  plain dark background — not 3D, not photorealistic, no cluttered / photographic /
  rainbow background, no visible text or watermark.
- **Always SFW** — never sexualised, revealing, or fan-servicey (she reads young).

### Negative prompt — token style (Stable Diffusion / Midjourney)

```
tall, adult, mature woman, elongated body, long realistic legs, sparkly huge moe
eyes, heavy eyelashes, winking, multiple accent colors, rainbow, colored hoodie,
patterned clothes, extra logos, brand logo, text, letters, watermark, signature,
orange ghost, tinted ghost, glowing-orange ghost, scary ghost, sheet ghost,
big-eyed ghost, multiple ghosts, ghost with hands, barefoot, boots, high heels,
white shoes, cluttered background, photographic background, rainbow background,
3d render, realistic, photorealistic, oil painting, extra fingers, deformed hands,
extra limbs, bad anatomy, lowres, blurry, jpeg artifacts, sexualized, revealing
```

---

## Expression Variants (four base emotions + 6 extras)

Each prompt concatenates the variant text after `[Common Block]`.

### Neutral (base standing pose / icon reference)

```
[Common Block] + calm deadpan expression, straight mouth, a cool self-possessed
unbothered air (quietly strong-willed, never fawning), direct steady gaze at the
viewer, relaxed neutral stance with one hand loosely in the hoodie pocket, the ghost
sprite hovering quietly, the cursor emblem in a steady soft-orange blink
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
petite black-bob girl reduced to a clean minimal logo mark: her blunt-bangs bob
silhouette wrapped around the orange shell-prompt ">|" mark (a ">" chevron + block
cursor) as the signature glyph, flat 2-color (black + #E8A33D), app-icon ready,
centered, no background detail
```

> Abstracted version, easy to reuse as an app icon / favicon. The `>▮` mark is the
> shared brand glyph (chest emblem + sneakers + logo), so keep it the anchor.

**Mark only — the pure emblem**

```
the orange shell-prompt ">|" mark (a ">" chevron + block cursor) as a standalone
logo, bold crisp geometry, flat 2-color (dark tile + #E8A33D), app-icon / favicon
ready, centered, no background detail
```

> The `>▮` glyph by itself — the tightest reduction, for favicons / tiny sizes.

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

## Wallpaper

Desktop / lock-screen wallpapers — including Noa's own `background-image` (the PNG
rendered behind the terminal grid; see `docs/specs/background-image.md` and the
directory-slideshow `docs/specs/live-wallpaper.md`). Finished wallpapers live in
`assets/wallpapers/`.

Wallpapers are **not stickers**. The character sits small and off-center, the frame
is wide and cinematic, and a large calm area is left empty — for desktop icons, the
menu bar / dock, and (when used as a terminal background) so the monospace text
stays readable on top. The shared Negative Prompt still applies.

### Wallpaper Block (use instead of the Common Block)

```
wide cinematic wallpaper illustration, a petite youthful girl (~5.5 heads tall, balanced) with
a sleek jet-black bob and full blunt bangs, large dark deadpan eyes with a quiet
strong-willed gaze, a very oversized black hoodie with an orange shell-prompt ">|"
emblem on the chest, black crew socks and chunky black sneakers with orange accents,
a white die-cut sticker outline, placed small toward one third of the frame (rule of
thirds), Ember her small round pure-white ghost sprite (chubby, two big round black
eyes with tiny catchlights, a small oval mouth and tiny rosy cheeks, cute) nearby
(matte white body with a cool soft-white glow, never orange or tinted — the warm
accent belongs only to the girl, not the ghost), dark charcoal atmosphere with
deep negative space, a single warm terminal-orange
(#E8A33D) accent, ambient depth and soft volumetric light, clean cel-shaded
subject over a richly rendered environment, no text, generous empty area for
desktop icons, 4k desktop wallpaper, aspect ratio {AR}
```

### Wallpaper Negative Prompt (append to the shared one)

```
orange ghost, orange ghost sprite, colored ghost, orange-tinted ghost, ghost
glowing orange, ghost matching the accent color, warm-tinted ghost
```

> The scene's warm light and the "single orange accent" keep bleeding onto Ember.
> Pin her white in the Wallpaper Block **and** exclude the orange ghost here, so
> the `#E8A33D` accent stays on the girl only. If it still tints, drop the word
> "glowing" from Ember's description and render her as a flat matte-white shape.
> On **GPT-Image-2** (no negative field) add this as a sentence, e.g. "the ghost is
> pure white — it must not pick up any orange from the scene light."

> Swap in `[Wallpaper Block]` wherever the other sheets say `[Common Block]`: any
> Expression or Scene variant above becomes a wallpaper by prepending this block
> instead — e.g. `[Wallpaper Block] + {Focused / Working variant}`. That's how the
> existing `assets/wallpapers/*.png` map back to the expression set.

### Aspect ratios ({AR})

| Target | AR | Note |
|--------|------|------|
| Desktop 16:9 | `16:9` | 1080p / 1440p / 4K (3840×2160) |
| Mac 16:10 | `16:10` | MacBook / Studio Display native |
| Ultrawide | `21:9` | keep the subject in one third, code-glow across the rest |
| Phone lock-screen | `9:19.5` | vertical reframe; subject low, empty dark above the clock |
| Terminal background | `16:10` | pair low-detail scenes with a low `background-opacity`; see below |

> Tool syntax: Midjourney `--ar 16:9`; SD / DALL·E set width×height to match.
> Render at or above the display's native resolution.

### Variants

**Hero desk — canonical desktop**

```
[Wallpaper Block] + she sits at a dark desk on the right third, faint orange
monospace code glow on her face, Ember hovering at her shoulder, a wide calm dark
wall filling the left two thirds, warm rim light, cozy late-night studio,
aspect ratio 16:10
```

> The default desktop shot. The left area stays empty for icons.

> **Hero desk variations** — all keep the subject on one third and a large calm
> empty area for icons; they vary time of day, camera angle, the subject's side,
> season, and desk setup. Lock the canonical shot's seed first, then run these.

**Hero desk — dawn shift (mirror, subject left)**

```
[Wallpaper Block] + she sits at a dark desk on the left third, cool pale dawn
light seeping through a blind behind her, warm orange screen glow on her hands,
Ember dozing on the desk, an empty calm wall filling the right two thirds, the
quiet after an all-nighter, aspect ratio 16:10
```

> Mirror of the canonical shot — subject left, icons on the right.

**Hero desk — multi-monitor command center**

```
[Wallpaper Block] + she sits center-right before a curved wall of dim terminal
monitors, faint orange code scrolling across them, Ember drifting between the
screens, the foreground desk in soft shadow, a dark ceiling void above for
negative space, focused command-center mood, aspect ratio 21:9
```

**Hero desk — coffee break**

```
[Wallpaper Block] + she leans back in her chair on the right third, both hands
around a steaming orange-logo mug, eyes half-closed, Ember floating in the mug
steam, a paused terminal glowing soft on the desk, a wide calm dark room to the
left, cozy pause, aspect ratio 16:9
```

**Hero desk — top-down flat lay**

```
[Wallpaper Block] + high overhead top-down view of her dark desk, she rests her
head sideways on folded arms beside the keyboard, Ember curled in the far corner,
scattered notes and a glowing orange terminal, plenty of clean empty desk surface,
aspect ratio 16:10
```

> Bird's-eye framing; the empty desk surface is the icon area.

**Hero desk — city window night**

```
[Wallpaper Block] + she sits at a desk pushed against a floor-to-ceiling night
window on the right third, a vast bokeh city glittering warm-orange beyond the
glass, a faint reflection of her screen on the pane, Ember perched on the sill,
the dark room opening to the left for negative space, aspect ratio 16:10
```

**Hero desk — winter blanket (seasonal)**

```
[Wallpaper Block] + she sits at the desk on the right third wrapped in a dark
blanket over her hoodie, sweater-paw sleeves resting on the keyboard, a small warm
heater glow, Ember tucked into the blanket fold, snow drifting past a dim window,
an empty cool wall to the left, cozy winter night, aspect ratio 16:10
```

> Reskin the season by swapping the window: cherry blossoms, rain, or autumn leaves.

**Hero desk — foreground depth (keyboard bokeh)**

```
[Wallpaper Block] + shallow depth of field, a mechanical keyboard with faint
orange keycaps sharp in the foreground, she and Ember softly out of focus in the
mid-ground on the right, a dark bokeh room, cinematic framing, empty blurred space
on the left, aspect ratio 16:9
```

**Minimal dark — terminal-background-safe**

```
[Wallpaper Block] + extreme minimalism, tiny silhouette of the girl and Ember in
the far bottom-right corner, the rest a near-black charcoal gradient with a single
soft orange block-cursor glow, almost empty, heavy negative space, very low
contrast, aspect ratio 16:10
```

> Built for Noa's own `background-image`: dark and near-empty so terminal text
> stays legible even at a high `background-opacity`. Pair it with a low opacity.

**Night-city ultrawide**

```
[Wallpaper Block] + hood up, walking alone along the lower edge of a neon night
street, wet pavement reflecting warm-orange signage bokeh, Ember drifting beside
her like a balloon, a vast moody sky above for negative space, cinematic ultrawide,
aspect ratio 21:9
```

**Phone lock-screen**

```
[Wallpaper Block] + vertical composition, the girl and Ember small near the bottom,
a tall calm dark expanse above them for the clock and notifications, a few faint
orange code glyphs drifting upward, aspect ratio 9:19.5
```

**Rain-window mood**

```
[Wallpaper Block] + she watches rain streak down a large dark window from a chair
on the left, city lights blurred warm-orange outside, Ember pressed to the glass,
faint reflections of monospace code on the pane, quiet rainy-night calm, empty
glass area on the right of frame, aspect ratio 16:9
```

### Charm variants — surface Noa's many sides (energetic · cute · gap-appeal)

> The other wallpapers lean moody/late-night. These excavate her brighter, cuter,
> livelier facets — but **through her own lens**: gap-moe, not generic-cute (see
> `mascot-ip-bible.md` §1 Do/Don't). Keep her deadpan-that-cracks, her dry
> competitiveness, the sweater-paws / hood-up gags, and the devoted duo with Ember.
> Palette discipline still holds — dark icon-friendly base, a single `#E8A33D`
> accent, Ember pure-white — so the subject radiates personality while the frame
> stays a usable wallpaper. Any Expression variant above (Joy / Smug / Embarrassed
> / Surprised / Ease) also works: `[Wallpaper Block] + {that variant}`.

**Victory grin — benchmark win (energetic · triumphant)**

```
[Wallpaper Block] + a rare bright genuine grin with a small triumphant fist-pump
on the right third, the terminal on her desk flashing a fast passing benchmark,
Ember bouncing overhead trailing tiny white sparkles, a burst of energy against
the calm dark room to the left, her guard-down happy moment, aspect ratio 16:10
```

> Her deadpan cracking into a real smile — the gap-moe that sells the IP.

**Competitive fire (game-face)**

```
[Wallpaper Block] + sudden sharp competitive glare, leaning hard toward the screen
on the right third, eyes lit with orange reflection, sleeves shoved up, a "3..2..1"
countdown glowing on the terminal, Ember hyped beside her with motion streaks, a
charged dark arena opening to the left, aspect ratio 16:9
```

> "Flat face → competitive glare the instant a contest starts." Her secretly-hates-losing side.

**Ember boop (cute · buddies)**

```
[Wallpaper Block] + a playful beat — she boops Ember the pure-white ghost with one
fingertip, both mid-bounce, a soft warm half-smile, small sparkles between them,
Ember beaming back, the duo drifting low-right with a wide calm dark space above,
their devoted-duo charm, aspect ratio 16:10
```

**Instant-ramen delight (cute · hungry)**

```
[Wallpaper Block] + she cradles a steaming cup of instant ramen with sweater-paw
sleeves on the right third, a tiny delighted sparkle breaking her deadpan, warm
steam curling up, Ember sniffing the bowl, a calm dark kitchen nook to the left,
her one-perfect-snack joy, aspect ratio 16:10
```

**Lo-fi vinyl vibe (good mood)**

```
[Wallpaper Block] + eyes closed with a content little head-bob, big headphones on,
a vinyl record spinning on the desk, faint floating music notes, one sweater-paw
tapping the beat, Ember swaying along, a wide dark wall for icons on the left,
cozy groove, aspect ratio 16:9
```

**Embarrassed hood-up (bashful · gap)**

```
[Wallpaper Block] + caught off guard by a compliment, she yanks her hood halfway
up with a strong blush, eyes darting aside, a tiny flustered pout, Ember peeking
teasingly from behind her shoulder, the pair on the right third, calm dark space
to the left, her signature embarrassed gag, aspect ratio 16:10
```

**Prickly-but-kind (soft-hearted)**

```
[Wallpaper Block] + a flat "whatever" face on the right third while she quietly
slides half her snack toward Ember, a faint soft blush betraying her, Ember
delighted, a small warm desk light, a wide calm dark area to the left, her
says-she-doesn't-care-then-does-the-kind-thing charm, aspect ratio 16:10
```

**Chair spin (energetic)**

```
[Wallpaper Block] + spinning gleefully in an office chair, arms flung out, hood
flying back, a rare wide grin, soft motion-blur streaks, Ember whirling around her
trailing white sparkles, an empty dark room giving room for the motion and for
icons, aspect ratio 16:9
```

---

## Operational Notes

- **Consistency tip**: Lock the Neutral pose first with a fixed seed, then run every other variant via i2i (image-to-image) so the art style matches.
- **Set production**: A one-image expression pack (3×3 grid) prompt is also possible.
- **Color**: Restrict the accent to the single terminal-orange `#E8A33D` (multiple accents scatter the look).

## Reference: Portrait Version (single illustration, not a mascot)

```
anime-style portrait of a petite, youthful girl with a sleek jet-black bob and
full blunt bangs, large dark deadpan eyes with a quiet strong-willed gaze looking
directly at the viewer, subtle flat expression (introverted, reserved), wearing a
very oversized black hoodie with an orange shell-prompt ">|" (a chevron plus a
block cursor) emblem on the chest, slightly slouched posture but confident
presence, a single warm terminal-orange (#E8A33D) accent, dark UI aesthetic, faint
monospace glyphs and a blinking block-cursor motif glowing softly in the
background, a small pure-white ghost (Ember) near her shoulder as a hidden motif,
moody low-key lighting, muted dark palette with a single orange highlight, clean
cel-shaded rendering, high quality, 4k
```
