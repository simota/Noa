# Image gallery

A display-only example of the `image` component. It shows three images
packaged in `assets/` (a PNG, a JPEG and a lossless WebP, all generated for
this example) at the preset sizes, in an explicit wide box, as a circle, and
one image chosen by the agent's data. It has no actions.

## Data contract

| Field | Meaning |
| --- | --- |
| `featured` | Asset ID of the image in "Chosen by data": `logo`, `landscape` or `pattern` |
| `caption` | Caption shown below it; also that image's alt text (1–200 characters) |

`featured` can only name a declared asset: its schema `enum` lists the asset
IDs, and validation rejects an enum entry that is not declared in `assets`.
An agent switches images by publishing new data; it cannot add an image.

## Validate and preview

Run from the repository root with `$NOA_BIN`, the installed `Noa` or a build.
Build `widget-host` first, as described in
[Repository widgets](../../docs/REPOSITORY_WIDGETS.md).

```sh
widget_noa=${NOA_BIN:-Noa}
package=.noa-widgets/image-gallery
"$widget_noa" +ctl widget validate "$package/widget.json"
for state in landscape pattern; do
  "$widget_noa" +ctl widget validate "$package/widget.json" \
    --data-file "$package/fixtures/$state.json" || exit 1
done
"$widget_noa" +ctl widget preview "$package/widget.json" \
  --data-file "$package/fixtures/pattern.json" \
  --snapshot "$(mktemp -d)/image-gallery.png" --size 960x720
```

`validate` lists each asset with its format, dimensions and content digest.
The definition digest covers those bytes: editing an image is a new
definition, and existing live instances keep the images they opened with.
