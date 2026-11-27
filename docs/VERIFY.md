# Inspect image evidence in Verify

The evidence gallery and criterion threads show native thumbnails for stored
PNG, JPEG, and static WebP evidence. A worker reads the saved copy without
following symlinks, verifies its recorded byte count, digest, and media type,
then decodes it. Loading and decoding do not run on the live window's UI
thread. A changed, missing, invalid, or unsupported image shows a reason or
its format label while the existing **開く** action remains available.

The thumbnail is labeled **縮小表示** with the original pixel dimensions.
Select it, or choose **拡大プレビュー**, to inspect the verified original in
a separate native window. **全体を表示** fits it in the window without
enlarging small images; **100% / 原寸** displays its pixels at their original
size, with horizontal and vertical scrolling. Escape closes that window.
Opening a preview does not run an agent or change the verification record.

**画像を比較** still opens the scoped, self-contained browser comparison.
GIF, animated PNG/WebP, and videos retain the external viewer/player path.
The native inspector rereads and verifies the saved copy when it opens, so a
file changed since its thumbnail loaded is refused with an explanation.

## Resource limits

Native decoding accepts at most 15 MiB per stored image, 8192 pixels on either
axis, and 16,777,216 pixels in total. The inspector also checks the GPU's
texture-dimension limit before upload. Files beyond those limits remain
available through the existing evidence actions.

The live gallery decodes up to 16 images in its current filtered scope using
one worker. Changing the filter selects another scope; repeated frames do
not enqueue the same images again. A queued batch holds at most 16 requests,
and completed results are also bounded. Thumbnails sample at most 96×64
pixels and join identical adjacent pixels before drawing. No more than 24
thumbnail instances are painted in one rebuilt view, so repeated references
to the same evidence cannot create unlimited drawing work.

Decoded previews remain in memory. They are never written over the original
evidence, and opening them does not register a new verification outcome.
