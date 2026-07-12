# Spec: Kitty Graphics Protocol

## Metadata

- slug: `kitty-graphics`
- title: Kitty graphics protocol (image transfer, display, deletion, Unicode placeholder)
- status: `implemented` (Phase 5 / Wave4)
- owner: simota
- Ghostty analog: `terminal/kitty/graphics_*.zig`, `terminal/kitty/graphics_unicode.zig`
- Upstream spec: <https://sw.kovidgoyal.net/kitty/graphics-protocol/>

A protocol for transferring and displaying images in the terminal. Used by `kitten icat` / `timg -pk` / `notcurses` and similar clients.
Control data rides on an APC (`ESC _ G <control> ; <base64 payload> ESC \`).

## Architecture (layer responsibilities)

```
pty bytes
  └ noa-vt   Parser: ESC _ → APC bounded capture(≤1 MiB) → Action::ApcDispatch
      └ Stream: leading 'G' → kitty_graphics::parse → Handler::kitty_graphics(KittyGraphicsCommand)
          └ noa-grid Terminal:
                kitty::ImageStore   — image data (cross-screen, global quota)
                Screen::kitty_placements — placements (per-screen, alt separated)
                responses → pending_writes (existing pty writer path)
              └ FrameSnapshot: projects visible placements + referenced images
                  └ noa-render image_layer: id→wgpu texture cache + z 3-band drawing
```

- Control-data parsing is in **noa-vt** (`kitty_graphics.rs`, a pure-function module on par with `sgr.rs`).
- Image decoding, state, and responses are in **noa-grid** (`kitty.rs` / `terminal.rs`).
- Projection and rendering are in **noa-render** (`snapshot.rs` / `image_layer.rs` / `shaders/image.wgsl`).

## Coverage

### Actions (`a=`)

| Value | Meaning | Supported |
|---|---|---|
| `t` | Transfer only | ✅ |
| `T` | Transfer and display immediately | ✅ |
| `p` | Display an already-transferred image (put) | ✅ |
| `d` | Delete image/placement | ✅ (see delete specifiers below) |
| `q` | Query (validate only, no storage, respond) | ✅ |
| `f` / `a` / `c` | Animation frame/control | ❌ `EUNSUPPORTED` |

### Format (`f=`)

- `f=24` (RGB, 3 bytes/px) → expanded to RGBA.
- `f=32` (RGBA, 4 bytes/px, default).
- `f=100` (PNG) → via the `png` crate. RGB/RGBA/grayscale/grayscale+alpha are normalized to RGBA8.
  16-bit samples are rounded to the high byte. **Palette PNGs are not supported** (`EBADPNG`).

### Medium (`t=`)

- `t=d` (direct, default): payload = base64-encoded image bytes. `o=z` (zlib) decompression is supported.
- `t=f` (file): payload = base64-encoded **absolute path**. `canonicalize` → must be a regular file → partial read via `S=`/`O=`.
- `t=t` (temp file): in addition to `t=f`, accepted only if the canonical path is under a temp directory
  (`$TMPDIR` / `/tmp` / `/dev/shm` / `/var/tmp`), or the path contains `tty-graphics-protocol`;
  deleted best-effort after reading. Returns `EINVAL` if the condition is not met.
- `t=s` (POSIX shared memory): ❌ `EUNSUPPORTED`.

### Chunked transfer (`m=1`)

Only one transfer in flight at a time (per the kitty spec). The first chunk's control data drives the final
decision, and continuation chunks only concatenate the payload. If another graphics command arrives mid-transfer,
the transfer is discarded and the new command is processed. `full_reset` (RIS) also discards it.

### ID assignment and responses

- `i=` specified → uses that id (overwriting transfer bumps the epoch to invalidate the texture cache).
- `i=0, I=n` → auto-assigns an id and reflects the assigned id in the response.
- `i=0 ∧ I=0` → auto-assigns an id but **sends no response at all** (kitty behavior).
- Response format: `ESC _ G i=<id>[,I=<n>][,p=<pid>] ; OK ESC \` / errors are `; E<code>:<message>`.
- Suppression: `q=1` suppresses OK; `q=2` also suppresses errors.

### Display (placement)

- `c=`/`r=` scale by cell; if unspecified, uses `ceil(display px / cell px)`.
- `x,y,w,h` crops the image; `X=`/`Y=` offsets the pixel position within the starting cell.
- `z=` sets the z-index (see bands below). `C=1` disables cursor movement.
- Placement anchors are **session-absolute rows** (the same approach as shell marks). Normal scrolling follows
  without transformation; rows dropped from scrollback are lazily cleaned up during snapshot generation.
  Region scroll / IL / DL delete intersecting placements as a v1 approximation.

### Deletion (`a=d`, `d=`)

Supports `a`(all) / `i`(id) / `n`(number) / `c`(cursor) / `p`(cell) / `q`(cell+z) / `r`(id range) /
`x`(column) / `y`(row) / `z`(z). Uppercase specifiers also free the image data once no placement
referencing that image remains on any screen. `d=f`/`F` (animation) is `EUNSUPPORTED`.
`ED 2` (screen erase) deletes intersecting placements; RIS deletes everything.

### Unicode placeholder (`U=1`)

A placement with `U=1` is stored as a virtual placement only (not drawn directly). The client prints cells with
the base scalar `U+10EEEE` and embeds the drawing position in the cell style:

- **Foreground color** → low bits of the image id (`Palette(n)` → 8 bits, `Rgb` → 24 bits).
- **1st combining diacritic** → image row.
- **2nd combining diacritic** → image column.
- **3rd combining diacritic** → high byte of the image id.
- **Underline color** → placement id (0 if omitted).

If row/column/high byte are omitted, they are inferred from the immediately preceding cell on the same screen row
(row and high byte carry over, column is +1). The row/column mapping table embeds kitty's
`rowcolumn-diacritics` (combining class 230, no decomposition mapping, 297 entries, from Unicode 6.0.0) as a
sorted array in ascending codepoint order, mapped to values via binary search
(`crates/noa-grid/src/kitty_placeholder.rs`). Consecutive column runs sharing the same (image id, placement id,
image row) are merged into a single quad, and the src sub-rectangle against the virtual placement's rows×cols
virtual grid is computed. Placeholder cells are excluded from glyph rendering, so only the image is visible.

## Z-band drawing

Within the same render pass, drawing interleaves with the cell pass, compositing placements into 3 bands by `z`:

1. `z < -2^30` — **below** the cell background.
2. cell background pass.
3. `-2^30 ≤ z < 0` — above the background, below the text.
4. cell glyph/decoration pass.
5. `z ≥ 0` — **above** the text (but below UI overlays).

## Quota

- Per-image dimension cap `MAX_IMAGE_DIM = 10_000` (width and height each, matching Ghostty). Exceeding it → `EFBIG`.
- Total decoded RGBA cap `TOTAL_BYTES_LIMIT = 320 MB` (kitty/Ghostty default). When exceeded, images without
  visible placements are evicted first in ascending seq order, then the oldest images if that's not enough.
- The renderer-side texture cache has a separate 512 MB / 300-frame LRU.
- The post-inflate size of `o=z` is also guarded by the single-image cap (to prevent zip bombs). APC capture is
  capped at 1 MiB; on overflow it is not discarded but dispatched with a `truncated` flag and responded to with `EFBIG`.

## Unsupported (response codes)

| Feature | Response |
|---|---|
| Animation (`a=f`/`a`/`c`, `d=f`/`F`) | `EUNSUPPORTED` |
| Shared memory (`t=s`) | `EUNSUPPORTED` |
| Palette PNG | `EBADPNG` |

Error code list: `EINVAL` (invalid request) / `EFBIG` (too large / truncated) / `ENODATA` (size mismatch) /
`EBADPNG` / `ENOENT` (file not found) / `EUNSUPPORTED`.

## Manual verification steps

```bash
kitten icat --detect-support        # detect support (should get a response)
kitten icat path/to/image.png       # display an image
kitten icat --clear                 # clear everything
# scroll follow: after icat, stream output and confirm the image scrolls up with the text
tmux new; kitten icat image.png     # inside tmux (with passthrough configured)
timg -pk image.png                  # display with a different client
```

Unicode placeholder can be verified with `kitten icat --unicode-placeholder image.png`.

## Open items (need cross-checking against real kitty)

- Final cursor position after displaying an image (pending_wrap handling when reaching the right edge).
- Image movement rules during region scroll (v1 approximates by deleting intersecting placements).
- The relationship between `ED 2`/`EL` and images (this implementation follows Ghostty parity).
