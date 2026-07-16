//! Font discovery (font-kit) and scaled font metrics (swash).

use font_kit::family_name::FamilyName;
use font_kit::font::Font;
use font_kit::handle::Handle;
use font_kit::properties::{Properties, Style, Weight};
use font_kit::source::Source;
use font_kit::source::SystemSource;
use swash::FontRef;

use crate::{FontConfig, FontError};

/// Backing storage for a font file: memory-mapped when loaded from disk
/// (the common case), heap-owned when font-kit hands us in-memory bytes.
///
/// Mapping instead of `fs::read`ing keeps system fonts (Apple Color Emoji
/// alone is ~190 MB) out of the process's dirty footprint: file-backed pages
/// are clean, reclaimable, and shared across every mapping of the same file —
/// including the one CoreText holds for other apps.
pub enum FontBytes {
    Owned(Vec<u8>),
    Mapped(memmap2::Mmap),
    /// Bytes compiled into the binary's read-only data segment via
    /// `include_bytes!` — the embedded Symbols Nerd Font Mono fallback (see
    /// `embedded_symbols_nerd_font_face`). Already resident and shared like a
    /// `Mapped` file, so unlike `Owned` it costs no heap copy.
    Static(&'static [u8]),
}

impl std::ops::Deref for FontBytes {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match self {
            FontBytes::Owned(bytes) => bytes,
            FontBytes::Mapped(map) => map,
            FontBytes::Static(bytes) => bytes,
        }
    }
}

impl PartialEq for FontBytes {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

impl std::fmt::Debug for FontBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self {
            FontBytes::Owned(_) => "Owned",
            FontBytes::Mapped(_) => "Mapped",
            FontBytes::Static(_) => "Static",
        };
        write!(f, "FontBytes::{kind}({} bytes)", self.len())
    }
}

impl From<Vec<u8>> for FontBytes {
    fn from(bytes: Vec<u8>) -> Self {
        FontBytes::Owned(bytes)
    }
}

/// Raw font file bytes plus the face index within a collection.
///
/// swash's [`FontRef`] borrows these bytes, so [`crate::FontGrid`] keeps the
/// [`FontBytes`] owned alongside the derived `FontRef`.
pub struct FontData {
    pub bytes: FontBytes,
    pub index: usize,
}

impl FontData {
    /// Borrow a swash [`FontRef`] over the owned bytes.
    pub fn font_ref(&self) -> Result<FontRef<'_>, FontError> {
        FontRef::from_index(&self.bytes, self.index).ok_or(FontError::Parse)
    }
}

pub struct FontStack {
    faces: Vec<FontData>,
    regular_faces: Vec<usize>,
    bold_faces: Vec<usize>,
    italic_faces: Vec<usize>,
    bold_italic_faces: Vec<usize>,
    native_bold_face: Option<usize>,
    native_italic_face: Option<usize>,
    native_bold_italic_face: Option<usize>,
}

impl FontStack {
    pub fn new(
        primary: FontData,
        bold_primary: Option<FontData>,
        italic_primary: Option<FontData>,
        bold_italic_primary: Option<FontData>,
        fallbacks: Vec<FontData>,
    ) -> Self {
        let mut faces = Vec::with_capacity(
            1 + usize::from(bold_primary.is_some())
                + usize::from(italic_primary.is_some())
                + usize::from(bold_italic_primary.is_some())
                + fallbacks.len(),
        );
        let primary_index = push_unique_face(&mut faces, primary);
        let native_bold_face = push_native_style_face(&mut faces, primary_index, bold_primary);
        let native_italic_face = push_native_style_face(&mut faces, primary_index, italic_primary);
        let native_bold_italic_face =
            push_native_style_face(&mut faces, primary_index, bold_italic_primary);

        let fallback_faces: Vec<_> = fallbacks
            .into_iter()
            .map(|face| push_unique_face(&mut faces, face))
            .collect();

        let regular_faces = stack_for_style(primary_index, None, &fallback_faces);
        let bold_faces = stack_for_style(primary_index, native_bold_face, &fallback_faces);
        let italic_faces = stack_for_style(primary_index, native_italic_face, &fallback_faces);
        let bold_italic_faces =
            stack_for_style(primary_index, native_bold_italic_face, &fallback_faces);

        Self {
            faces,
            regular_faces,
            bold_faces,
            italic_faces,
            bold_italic_faces,
            native_bold_face,
            native_italic_face,
            native_bold_italic_face,
        }
    }

    pub fn primary(&self) -> &FontData {
        &self.faces[self.regular_faces[0]]
    }

    pub fn faces(&self) -> &[FontData] {
        &self.faces
    }

    pub fn face_indices_for_style(&self, style: FontStyle) -> &[usize] {
        match style {
            FontStyle::Regular => &self.regular_faces,
            FontStyle::Bold => &self.bold_faces,
            FontStyle::Italic => &self.italic_faces,
            FontStyle::BoldItalic => &self.bold_italic_faces,
        }
    }

    /// Append a dynamically-discovered fallback face (from the macOS CoreText
    /// cascade — see [`cascade_fallback_face`]) to the stack, making it
    /// reachable for every style, and return its face index.
    ///
    /// Deduplicates against faces already loaded (via [`push_unique_face`]),
    /// so repeated cascade hits for the same font file share one face rather
    /// than reloading its bytes. The index is stable for the rest of this
    /// stack's life (faces are only ever appended, never reordered), which is
    /// what lets `FaceId` stay a plain index into `faces`.
    pub fn push_dynamic_fallback(&mut self, face: FontData) -> usize {
        let idx = push_unique_face(&mut self.faces, face);
        for list in [
            &mut self.regular_faces,
            &mut self.bold_faces,
            &mut self.italic_faces,
            &mut self.bold_italic_faces,
        ] {
            if !list.contains(&idx) {
                list.push(idx);
            }
        }
        idx
    }

    pub fn is_native_style_face(&self, face_index: usize, style: FontStyle) -> bool {
        // A shared fallback face (emoji/Nerd Font/CJK, loaded regular-only —
        // see `load_font_stack` — and reused across every style stack via
        // `stack_for_style`) must never be synthetically slanted/emboldened,
        // no matter which style resolved it: Ghostty applies synthetic
        // bold/italic only to the primary family's own regular face, never to
        // a dynamically-discovered fallback (`CodepointResolver.zig`
        // `getIndex` + `Collection.zig` `completeStyles`). Treating it as
        // "native" here is exactly the signal callers (`synthesis_for`) use
        // to skip synthesis.
        if self.is_fallback_face(face_index) {
            return true;
        }
        match style {
            FontStyle::Regular => true,
            FontStyle::Bold => self.native_bold_face == Some(face_index),
            FontStyle::Italic => self.native_italic_face == Some(face_index),
            FontStyle::BoldItalic => self.native_bold_italic_face == Some(face_index),
        }
    }

    /// Whether `face_index` belongs to the fallback stack (emoji / Nerd Font /
    /// CJK from [`load_font_stack`], or a macOS CoreText cascade hit pushed by
    /// [`FontStack::push_dynamic_fallback`]) rather than one of the primary
    /// family's four style slots (regular/bold/italic/bold-italic).
    pub fn is_fallback_face(&self, face_index: usize) -> bool {
        face_index != self.regular_faces[0]
            && Some(face_index) != self.native_bold_face
            && Some(face_index) != self.native_italic_face
            && Some(face_index) != self.native_bold_italic_face
    }

    /// Whether `face_index`'s family is a Nerd Font — the source of
    /// Ghostty's generated icon/Powerline codepoint ranges
    /// (`nerd_font_attributes.zig`) that stay cell-constrained even though
    /// ordinary text glyphs are not (see `raster::rasterize_with_variations`'s
    /// `fit_width` doc comment). Read from the face's own name-table family
    /// string rather than tracked separately at stack construction, so this
    /// also covers a Nerd Font resolved dynamically via the macOS cascade.
    pub fn is_icon_fallback_face(&self, face_index: usize) -> bool {
        let Some(font_data) = self.faces.get(face_index) else {
            return false;
        };
        let Ok(font) = font_data.font_ref() else {
            return false;
        };
        let strings = font.localized_strings();
        [swash::StringId::Family, swash::StringId::TypographicFamily]
            .into_iter()
            .filter_map(|id| strings.find_by_id(id, None))
            .any(|name| is_nerd_font_family_name(&name.chars().collect::<String>()))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontStyle {
    Regular,
    Bold,
    Italic,
    BoldItalic,
}

impl FontStyle {
    pub fn from_bold_italic(bold: bool, italic: bool) -> Self {
        match (bold, italic) {
            (false, false) => Self::Regular,
            (true, false) => Self::Bold,
            (false, true) => Self::Italic,
            (true, true) => Self::BoldItalic,
        }
    }
}

fn stack_for_style(
    primary_index: usize,
    native_style_index: Option<usize>,
    fallback_faces: &[usize],
) -> Vec<usize> {
    let mut stack = Vec::with_capacity(1 + fallback_faces.len());
    stack.push(native_style_index.unwrap_or(primary_index));
    stack.extend_from_slice(fallback_faces);
    stack
}

fn push_unique_face(faces: &mut Vec<FontData>, face: FontData) -> usize {
    if let Some((idx, _)) = faces
        .iter()
        .enumerate()
        .find(|(_, existing)| font_data_matches(existing, &face))
    {
        return idx;
    }

    let idx = faces.len();
    faces.push(face);
    idx
}

fn push_native_style_face(
    faces: &mut Vec<FontData>,
    primary_index: usize,
    face: Option<FontData>,
) -> Option<usize> {
    let face = face?;
    if font_data_matches(&faces[primary_index], &face) {
        return None;
    }
    Some(push_unique_face(faces, face))
}

fn font_data_matches(a: &FontData, b: &FontData) -> bool {
    a.index == b.index && a.bytes == b.bytes
}

/// Enumerate every font family name known to the system source, sorted and
/// deduplicated. Backs the `noa +list-fonts` CLI action.
pub fn list_families() -> Result<Vec<String>, FontError> {
    let mut families = SystemSource::new()
        .all_families()
        .map_err(|_| FontError::Enumerate)?;
    families.sort();
    families.dedup();
    Ok(families)
}

/// Discover the default coding font and load its raw bytes.
///
/// On macOS, prefer `Menlo` first: it is the standard terminal/coding face and
/// the rest of `load_font_stack` still adds emoji, Nerd Font, CJK, and CoreText
/// cascade fallbacks for multibyte coverage. Other platforms keep the generic
/// monospace lookup, with `Menlo` retained as a compatibility fallback.
fn load_monospace_from_source(source: &SystemSource) -> Result<FontData, FontError> {
    let properties = properties_for_style(FontStyle::Regular);
    load_default_monospace_family(source, &properties, Some(FontStyle::Regular))
        .or_else(|| load_generic_monospace_family(source, &properties, Some(FontStyle::Regular)))
        .or_else(|| {
            load_menlo_compatibility_fallback(source, &properties, Some(FontStyle::Regular))
        })
        .ok_or(FontError::NoFont)
}

/// Discover the font stack described by `font_cfg`, falling back to the
/// platform default coding font (see [`load_monospace_from_source`]) when
/// `font_cfg.families` is empty or none of the configured families resolve.
///
/// Configured regular/bold/italic families are resolved with font-kit's CSS
/// matcher. If a native style face is unavailable, the style stack falls back
/// to the regular primary and rasterization may synthesize the missing style.
pub fn load_font_stack(font_cfg: &FontConfig) -> Result<FontStack, FontError> {
    load_font_stack_with_primary(load_primary_font(font_cfg)?, font_cfg)
}

/// Resolve and load only the primary (regular) face for `font_cfg` — the
/// face [`Metrics`] (and thus the first window's pixel size) are computed
/// from. This is a small fraction of the full [`load_font_stack`] cost, so a
/// caller that needs cell metrics early (window sizing at startup) can load
/// the primary first, publish its metrics, and feed the face back into
/// [`load_font_stack_with_primary`] to finish discovery without re-resolving.
pub fn load_primary_font(font_cfg: &FontConfig) -> Result<FontData, FontError> {
    let source = SystemSource::new();
    match load_configured_primary(&source, font_cfg) {
        Some(primary) => Ok(primary),
        None => load_monospace_from_source(&source),
    }
}

/// Second stage of [`load_font_stack`]: style faces + the fallback cascade,
/// with the already-loaded `primary` from [`load_primary_font`]. `font_cfg`
/// must be the config the primary was resolved with.
pub fn load_font_stack_with_primary(
    primary: FontData,
    font_cfg: &FontConfig,
) -> Result<FontStack, FontError> {
    let source = SystemSource::new();
    let bold_primary = load_style_primary(&source, font_cfg, FontStyle::Bold);
    let italic_primary = load_style_primary(&source, font_cfg, FontStyle::Italic);
    let bold_italic_primary = load_style_primary(&source, font_cfg, FontStyle::BoldItalic);
    let mut fallbacks = Vec::new();

    // Probe for the system color-emoji family first so emoji codepoints
    // resolve to it rather than falling through to a tofu/blank glyph or a
    // mismatched fallback face (REQ-EMOJI-1). Same family-by-name lookup
    // mechanism as the primary/CJK discovery below; simply yields no match
    // on platforms/sandboxes without it, so the stack proceeds without an
    // emoji face rather than failing.
    push_some_face(
        &mut fallbacks,
        family_fallback_face(&source, emoji_fallback_family_name()),
    );

    // Private-use icons from Nerd Fonts are not present in the normal system
    // monospace/CJK stack. Keep these after emoji (so color emoji wins) but
    // before CJK (so PUA icons don't accidentally resolve to a CJK private
    // glyph when a Nerd Font candidate is installed).
    for family_name in nerd_font_fallback_family_names(&source) {
        push_some_face(&mut fallbacks, family_fallback_face(&source, &family_name));
    }

    // Permanent embedded fallback: guarantees Nerd Font PUA icon coverage
    // even when no Nerd Font family is installed system-wide (NFR-5/AC-21).
    // Kept after the installed-Nerd-Font loop above (an installed family
    // keeps winning) and before CJK below (so PUA never resolves to a CJK
    // private glyph) — see `embedded_symbols_nerd_font_face`.
    push_some_face(&mut fallbacks, embedded_symbols_nerd_font_face());

    // CJK fallbacks are NOT loaded here. Resolving the ~18 curated CJK
    // families/faces at startup faulted whole `.ttc` files into the page cache
    // (Hiragino Sans GB 44.8 MB, ヒラギノ角ゴシック W3 30.0 MB, AppleGothic
    // 29.2 MB, …) — ~100 MB of idle RSS even when no CJK glyph is ever drawn.
    // Instead a CJK codepoint that misses the stack pulls in exactly the one
    // font that covers it, lazily, via [`cjk_fallback_face_for`] (called from
    // `FontGrid` on a stack miss, ahead of the generic system cascade). The
    // priority order there is identical to the eager list this replaced, so
    // which font wins for a given CJK codepoint is unchanged.

    Ok(FontStack::new(
        primary,
        bold_primary,
        italic_primary,
        bold_italic_primary,
        fallbacks,
    ))
}

/// Try each configured family name in order, returning the first that
/// resolves to a loadable face. `None` (not an error) means the caller
/// should fall back to system monospace discovery.
fn load_configured_primary(source: &SystemSource, font_cfg: &FontConfig) -> Option<FontData> {
    let properties = properties_for_style(FontStyle::Regular);
    load_first_matching_family(source, &font_cfg.families, &properties, None)
}

fn load_style_primary(
    source: &SystemSource,
    font_cfg: &FontConfig,
    style: FontStyle,
) -> Option<FontData> {
    let properties = properties_for_style(style);
    let explicit_families = match style {
        FontStyle::Regular => &font_cfg.families,
        FontStyle::Bold => &font_cfg.families_bold,
        FontStyle::Italic => &font_cfg.families_italic,
        FontStyle::BoldItalic => &font_cfg.families_bold_italic,
    };

    load_first_matching_family(source, explicit_families, &properties, Some(style))
        .or_else(|| {
            load_first_matching_family(source, &font_cfg.families, &properties, Some(style))
        })
        .or_else(|| load_system_style_primary(source, &properties, style))
}

fn load_system_style_primary(
    source: &SystemSource,
    properties: &Properties,
    style: FontStyle,
) -> Option<FontData> {
    load_default_monospace_family(source, properties, Some(style))
        .or_else(|| load_generic_monospace_family(source, properties, Some(style)))
        .or_else(|| load_menlo_compatibility_fallback(source, properties, Some(style)))
}

#[cfg(target_os = "macos")]
fn default_monospace_family_names() -> &'static [&'static str] {
    &["Menlo"]
}

#[cfg(not(target_os = "macos"))]
fn default_monospace_family_names() -> &'static [&'static str] {
    &[]
}

fn load_default_monospace_family(
    source: &SystemSource,
    properties: &Properties,
    required_style: Option<FontStyle>,
) -> Option<FontData> {
    default_monospace_family_names()
        .iter()
        .find_map(|family_name| load_title_family(source, family_name, properties, required_style))
}

fn load_generic_monospace_family(
    source: &SystemSource,
    properties: &Properties,
    required_style: Option<FontStyle>,
) -> Option<FontData> {
    let handle = source
        .select_best_match(&[FamilyName::Monospace], properties)
        .ok()?;
    let handle = match required_style {
        Some(style) => resolve_required_style(source, handle, style)?,
        None => handle,
    };
    load_valid_handle(handle)
}

fn load_menlo_compatibility_fallback(
    source: &SystemSource,
    properties: &Properties,
    required_style: Option<FontStyle>,
) -> Option<FontData> {
    if default_monospace_family_names().contains(&"Menlo") {
        return None;
    }
    load_title_family(source, "Menlo", properties, required_style)
}

/// Resolve one named family for `required_style` (or the regular cut when
/// `None`), preferring the no-slurp CoreText→mmap path on macOS
/// ([`mapped_font_data_for_family_style`]) and falling back to font-kit's
/// slurping selector off macOS, or on macOS when the mmap path can't resolve
/// the family/style (a font with no on-disk file, or a style whose native cut
/// this family lacks — where font-kit's `resolve_required_style` also returns
/// `None`, so the style is synthesized either way).
fn load_named_family_for_style(
    source: &SystemSource,
    family_name: &str,
    properties: &Properties,
    required_style: Option<FontStyle>,
) -> Option<FontData> {
    #[cfg(target_os = "macos")]
    if let Some(face) =
        mapped_font_data_for_family_style(family_name, required_style.unwrap_or(FontStyle::Regular))
    {
        return Some(face);
    }
    let handle = select_title_best_match(source, family_name, properties).ok()?;
    let handle = match required_style {
        Some(style) => resolve_required_style(source, handle, style)?,
        None => handle,
    };
    load_valid_handle(handle)
}

fn load_title_family(
    source: &SystemSource,
    family_name: &str,
    properties: &Properties,
    required_style: Option<FontStyle>,
) -> Option<FontData> {
    load_named_family_for_style(source, family_name, properties, required_style)
}

fn load_first_matching_family(
    source: &SystemSource,
    families: &[String],
    properties: &Properties,
    required_style: Option<FontStyle>,
) -> Option<FontData> {
    families.iter().find_map(|family_name| {
        load_named_family_for_style(source, family_name, properties, required_style)
    })
}

/// Confirm `handle` actually provides `style`, hardened against a font
/// backend that misreports `Font::properties()` identically for every static
/// face in a family (verified for Menlo and Helvetica on font-kit 0.14.3's
/// CoreText backend — Courier New and Hiragino Sans are unaffected there).
///
/// When the family's `properties()` are self-consistent (the common case),
/// `handle_supports_style` — font-kit's own CSS-properties check, which is
/// also what selected `handle` in the first place — decides, with a
/// PostScript-name cross-check as a defensive fallback if it still rejects
/// `handle`: unchanged behavior for families where `properties()` works.
/// When the family's `properties()` are detected unreliable, both that check
/// and font-kit's own selection are equally untrustworthy, so style is
/// resolved purely from each family member's PostScript name instead — read
/// back reliably even when `properties()` is not (e.g. "Menlo-Italic").
/// Returns `None` when no face in the family genuinely has the requested
/// style, either way.
fn resolve_required_style(
    source: &SystemSource,
    handle: Handle,
    style: FontStyle,
) -> Option<Handle> {
    let family_name = Font::from_handle(&handle).ok()?.family_name();
    let members = family_members_with_postscript_names(source, &family_name);

    // Every static face in the family reporting identical `properties()` is
    // never legitimate (a real Regular/Bold/Italic/BoldItalic cut always
    // differs in at least weight or slant) — it means this font backend
    // cannot be trusted to distinguish this family's faces at all. In that
    // case font-kit's own `select_best_match`, which chose `handle` using
    // those same properties, is just as unreliable as our `properties()`
    // check would be, so skip both and resolve purely by PostScript name.
    if properties_are_unreliable(&members) {
        return find_by_postscript_style(&members, style);
    }

    if handle_supports_style(&handle, style) {
        return Some(handle);
    }
    find_by_postscript_style(&members, style)
}

/// `(Handle, PostScript name)` for every loadable member of `family_name`.
fn family_members_with_postscript_names(
    source: &SystemSource,
    family_name: &str,
) -> Vec<(Handle, String)> {
    let Ok(family) = source.select_family_by_name(family_name) else {
        return Vec::new();
    };
    family
        .fonts()
        .iter()
        .filter_map(|handle| {
            let postscript_name = Font::from_handle(handle).ok()?.postscript_name()?;
            Some((handle.clone(), postscript_name))
        })
        .collect()
}

/// Whether `Font::properties()` is unreliable for this family: true when two
/// distinctly-PostScript-named static faces report identical properties
/// (verified for Menlo and Helvetica on font-kit 0.14.3's CoreText backend —
/// every one of Menlo's four static faces, Regular included, reads back as
/// `Style::Italic`/weight 400; Courier New and Hiragino Sans are unaffected).
fn properties_are_unreliable(members: &[(Handle, String)]) -> bool {
    let mut seen: Vec<Properties> = Vec::new();
    for (handle, _) in members {
        let Ok(font) = Font::from_handle(handle) else {
            continue;
        };
        let props = font.properties();
        if seen.contains(&props) {
            return true;
        }
        seen.push(props);
    }
    false
}

/// Find a member whose PostScript name's style suffix matches `style` (e.g.
/// "Menlo-Italic" for [`FontStyle::Italic`]) — the name-table-based
/// resolution path used by [`resolve_required_style`].
fn find_by_postscript_style(members: &[(Handle, String)], style: FontStyle) -> Option<Handle> {
    members
        .iter()
        .find(|(_, postscript_name)| postscript_name_matches_style(postscript_name, style))
        .map(|(handle, _)| handle.clone())
}

/// Whether a PostScript name's style suffix (e.g. "-Italic", "-BoldItalic")
/// matches `style` exactly — a bold-only request must not match a
/// "BoldItalic" face, and vice versa.
fn postscript_name_matches_style(postscript_name: &str, style: FontStyle) -> bool {
    let lower = postscript_name.to_ascii_lowercase();
    let wants_bold = matches!(style, FontStyle::Bold | FontStyle::BoldItalic);
    let wants_italic = matches!(style, FontStyle::Italic | FontStyle::BoldItalic);
    let has_bold = lower.contains("bold");
    let has_italic = lower.contains("italic") || lower.contains("oblique");
    has_bold == wants_bold && has_italic == wants_italic
}

fn select_title_best_match(
    source: &SystemSource,
    family_name: &str,
    properties: &Properties,
) -> Result<Handle, font_kit::error::SelectionError> {
    let family = [FamilyName::Title(family_name.to_string())];
    source.select_best_match(&family, properties)
}

fn load_valid_handle(handle: Handle) -> Option<FontData> {
    let data = handle_to_data(handle).ok()?;
    data.font_ref().is_ok().then_some(data)
}

fn properties_for_style(style: FontStyle) -> Properties {
    let mut properties = Properties::new();
    match style {
        FontStyle::Regular => {}
        FontStyle::Bold => {
            properties.weight(Weight::BOLD);
        }
        FontStyle::Italic => {
            properties.style(Style::Italic);
        }
        FontStyle::BoldItalic => {
            properties.weight(Weight::BOLD).style(Style::Italic);
        }
    }
    properties
}

fn handle_supports_style(handle: &Handle, style: FontStyle) -> bool {
    let Ok(font) = Font::from_handle(handle) else {
        return false;
    };
    let properties = font.properties();
    let wants_bold = matches!(style, FontStyle::Bold | FontStyle::BoldItalic);
    let wants_italic = matches!(style, FontStyle::Italic | FontStyle::BoldItalic);
    let has_bold = !wants_bold || properties.weight >= Weight::SEMIBOLD;
    let has_italic = !wants_italic || matches!(properties.style, Style::Italic | Style::Oblique);
    has_bold && has_italic
}

fn push_some_face(faces: &mut Vec<FontData>, face: Option<FontData>) {
    if let Some(face) = face {
        faces.push(face);
    }
}

/// Resolve the best normal-style face of `family_name` for the fallback
/// stack. On macOS this walks CoreText descriptors only — no font bytes are
/// read until the chosen file is mmapped — because font-kit's `select_*`
/// slurps every candidate file into the heap (Apple Color Emoji alone is
/// ~190 MB of transient allocation per lookup). font-kit remains the
/// portable/fallback path.
fn family_fallback_face(source: &SystemSource, family_name: &str) -> Option<FontData> {
    #[cfg(target_os = "macos")]
    if let Some(face) = mapped_font_data_for_family(family_name) {
        return Some(face);
    }
    let family = [FamilyName::Title(family_name.to_string())];
    let handle = Source::select_best_match(source, &family, &Properties::new()).ok()?;
    load_valid_handle(handle)
}

/// Resolve a PostScript name for the fallback stack; mmap-first like
/// [`family_fallback_face`].
fn postscript_fallback_face(source: &SystemSource, postscript_name: &str) -> Option<FontData> {
    #[cfg(target_os = "macos")]
    if let Some(face) = mapped_font_data_for_postscript_name(postscript_name) {
        return Some(face);
    }
    let handle = Source::select_by_postscript_name(source, postscript_name).ok()?;
    load_valid_handle(handle)
}

/// The macOS system color-emoji family name (REQ-EMOJI-1). `font-kit`'s
/// `select_family_by_name` resolves this the same way it resolves any other
/// installed family, whether the backing source is CoreText (macOS) or
/// another platform's font database (where it will simply not be found).
fn emoji_fallback_family_name() -> &'static str {
    "Apple Color Emoji"
}

fn nerd_font_fallback_family_names(source: &SystemSource) -> Vec<String> {
    let mut names: Vec<String> = [
        "Symbols Nerd Font Mono",
        "Symbols Nerd Font",
        "Hack Nerd Font Mono",
        "Hack Nerd Font",
        "Hack Nerd Font Propo",
        "JetBrainsMono Nerd Font Mono",
        "JetBrainsMono Nerd Font",
        "FiraCode Nerd Font Mono",
        "FiraCode Nerd Font",
        "CaskaydiaCove Nerd Font Mono",
        "CaskaydiaCove Nerd Font",
        "SauceCodePro Nerd Font Mono",
        "SauceCodePro Nerd Font",
        "MesloLGS NF",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();

    if let Ok(mut discovered) = source.all_families() {
        discovered.retain(|name| is_nerd_font_family_name(name));
        names.extend(discovered);
    }

    let mut deduped = Vec::with_capacity(names.len());
    for name in names {
        if !deduped.iter().any(|existing: &String| existing == &name) {
            deduped.push(name);
        }
    }
    deduped.sort_by_key(|name| (nerd_font_family_priority(name), name.to_ascii_lowercase()));
    deduped
}

fn is_nerd_font_family_name(name: &str) -> bool {
    name.contains("Nerd Font") || name.ends_with(" NF")
}

fn nerd_font_family_priority(name: &str) -> u8 {
    if name.contains("Symbols Nerd Font") {
        0
    } else if name.contains("Nerd Font Mono") || name.ends_with(" NF") {
        1
    } else if name.contains("Nerd Font Propo") {
        3
    } else {
        2
    }
}

/// The vendored "Symbols Nerd Font Mono" face (MIT, `ryanoasis/nerd-fonts`;
/// see `vendor/ATTRIBUTION.md`), compiled directly into the binary.
///
/// It carries no Latin/CJK letterforms — only Nerd Fonts' private-use-area
/// icon codepoints and a small set of shared symbols — so it can only ever
/// resolve codepoints the primary/emoji/CJK faces miss, which is what makes
/// it safe to keep permanently in the fallback stack (see
/// [`embedded_symbols_nerd_font_face`]'s call site in `load_font_stack`).
static EMBEDDED_SYMBOLS_NERD_FONT_MONO: &[u8] =
    include_bytes!("../vendor/SymbolsNerdFontMono-Regular.ttf");

/// Load the embedded Symbols Nerd Font Mono face (see
/// [`EMBEDDED_SYMBOLS_NERD_FONT_MONO`]) as a permanent, always-present Nerd
/// Font fallback — the guarantee behind `docs/specs/session-sidebar.md`'s
/// NFR-5/AC-21: the sidebar's own Nerd Font PUA glyphs (status dot
/// `U+F111`, `icon_glyph`'s project icons) render even on a machine with no
/// Nerd Font family installed, mirroring Ghostty's bundled-font coverage
/// guarantee.
///
/// Placed in `load_font_stack` after any installed Nerd Font (an installed
/// family — the user's own choice — keeps winning per
/// [`FontStack::face_indices_for_style`]'s first-match order) and before CJK
/// (so PUA codepoints never resolve to a CJK private glyph). Returns `None`
/// only if the embedded bytes somehow fail to parse, which would indicate a
/// corrupt vendor asset rather than a runtime condition.
fn embedded_symbols_nerd_font_face() -> Option<FontData> {
    let data = FontData {
        bytes: FontBytes::Static(EMBEDDED_SYMBOLS_NERD_FONT_MONO),
        index: 0,
    };
    data.font_ref().is_ok().then_some(data)
}

/// Ask the macOS CoreText cascade which installed font can render `ch`, and
/// load it as a [`FontData`].
///
/// This mirrors Ghostty's ultimate fallback: after the curated
/// emoji/Nerd/CJK stack ([`load_font_stack`]) misses, defer to the system's
/// own font-substitution machinery (`CTFontCreateForString`) so codepoints
/// that only a niche system font covers — e.g. `⏵` U+23F5, which resolves to
/// STIX Two Math — still render instead of showing tofu. Called lazily by
/// `FontGrid` on a stack miss and cached, so it never runs for codepoints the
/// curated stack already handles.
///
/// Returns `None` when nothing but the LastResort placeholder covers `ch`, so
/// the caller records the miss and falls back to a genuine tofu glyph.
#[cfg(target_os = "macos")]
pub(crate) fn cascade_fallback_face(ch: char) -> Option<FontData> {
    use core_foundation::base::{CFRange, TCFType};
    use core_foundation::string::{CFString, CFStringRef};
    use core_text::font::{CTFont, CTFontRef};

    // Present in the already-linked CoreText framework but not re-exported by
    // the `core-text` crate (commented out upstream), so declare it here.
    unsafe extern "C" {
        fn CTFontCreateForString(
            current_font: CTFontRef,
            string: CFStringRef,
            range: CFRange,
        ) -> CTFontRef;
    }

    // A representative monospace base for the cascade query. The substitute
    // CoreText returns for a given codepoint is effectively system-wide, so
    // the exact base font does not change which fallback ends up covering it.
    let base = core_text::font::new_from_name("Menlo", 12.0).ok()?;
    let string = CFString::new(&ch.to_string());
    let range = CFRange {
        location: 0,
        length: string.char_len(),
    };
    let substitute = unsafe {
        let raw = CTFontCreateForString(
            base.as_concrete_TypeRef(),
            string.as_concrete_TypeRef(),
            range,
        );
        if raw.is_null() {
            return None;
        }
        CTFont::wrap_under_create_rule(raw)
    };

    let postscript = substitute.postscript_name();
    // CoreText returns the LastResort font (its own styled hex-box art) when
    // nothing real covers the codepoint. Treat that as a miss so the glyph
    // stays a genuine tofu rather than swapping one box for another.
    if postscript.is_empty() || postscript.trim_start_matches('.').starts_with("LastResort") {
        return None;
    }

    // Resolve the substitute to loadable bytes + face index. Prefer the
    // direct mmap path (no whole-file read); fall back to font-kit's
    // PostScript-name lookup for faces without an on-disk file.
    let data = mapped_font_data_for_postscript_name(&postscript).or_else(|| {
        let source = SystemSource::new();
        let handle = Source::select_by_postscript_name(&source, &postscript).ok()?;
        load_valid_handle(handle)
    })?;
    if data.font_ref().is_err() {
        return None;
    }

    // Guarantee the returned face actually maps `ch`: font-kit's PostScript
    // lookup should hand back exactly CoreText's substitute, but verifying it
    // here lets the caller trust that a `Some` always covers the codepoint —
    // so its retry-after-push cannot loop re-probing CoreText every frame.
    let covers = data.font_ref().ok()?.charmap().map(ch) != 0;
    covers.then_some(data)
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn cascade_fallback_face(_ch: char) -> Option<FontData> {
    None
}

/// Lazily resolve the first curated CJK fallback face that covers `ch`, in
/// the exact priority order the eager stack used to load them
/// ([`cjk_fallback_postscript_names`] then [`cjk_fallback_family_names`]) — so
/// which font wins for a given CJK codepoint is unchanged from when the whole
/// CJK list was resolved at startup.
///
/// Called by `FontGrid` on a stack miss, *before* the generic macOS system
/// cascade ([`cascade_fallback_face`]), and its result is pushed into the
/// stack and cached — so no CJK font file is resolved or mapped until a
/// codepoint one of these fonts covers is actually drawn. Each candidate
/// resolves via the no-slurp CoreText→mmap path ([`postscript_fallback_face`]
/// / [`family_fallback_face`]); probing a candidate that does not cover `ch`
/// only faults that font's `cmap`, and the winning font stays memory-mapped so
/// only its touched glyph pages ever become resident.
///
/// Returns `None` when no curated CJK font covers `ch` (the caller then tries
/// the generic system cascade).
pub(crate) fn cjk_fallback_face_for(ch: char) -> Option<FontData> {
    let source = SystemSource::new();
    cjk_fallback_postscript_names()
        .iter()
        .find_map(|name| face_covering(postscript_fallback_face(&source, name), ch))
        .or_else(|| {
            cjk_fallback_family_names()
                .iter()
                .find_map(|name| face_covering(family_fallback_face(&source, name), ch))
        })
}

/// Return `face` only if it maps `ch` to a real (non-notdef) glyph — the
/// coverage gate for the lazy CJK priority walk in [`cjk_fallback_face_for`].
fn face_covering(face: Option<FontData>, ch: char) -> Option<FontData> {
    let face = face?;
    (face.font_ref().ok()?.charmap().map(ch) != 0).then_some(face)
}

fn cjk_fallback_postscript_names() -> &'static [&'static str] {
    #[cfg(target_os = "macos")]
    {
        &[
            "HiraginoSans-W3",
            "HiraginoSans-W4",
            "HiraginoSans-W5",
            "HiraKakuProN-W3",
            "HiraKakuPro-W3",
            "YuGothic-Regular",
            "YuGothicUI-Regular",
            "NotoSansCJKjp-Regular",
            "NotoSansMonoCJKjp-Regular",
            "HiraginoSansGB-W3",
            "AppleGothic",
        ]
    }
    #[cfg(not(target_os = "macos"))]
    {
        &[]
    }
}

fn cjk_fallback_family_names() -> &'static [&'static str] {
    &[
        "Hiragino Sans",
        "Hiragino Kaku Gothic ProN",
        "Yu Gothic",
        "Noto Sans CJK JP",
        "Noto Sans Mono CJK JP",
        "Hiragino Sans GB",
        "AppleGothic",
    ]
}

fn handle_to_data(handle: Handle) -> Result<FontData, FontError> {
    match handle {
        Handle::Memory { bytes, font_index } => {
            // font-kit's CoreText source slurps every selected font file into
            // memory and only ever returns `Handle::Memory` on macOS, so a
            // naive clone would keep e.g. Apple Color Emoji's ~190 MB resident
            // per FontGrid. Re-resolve the face to its on-disk file via its
            // PostScript name and map that instead; the slurped copy dies with
            // the handle.
            #[cfg(target_os = "macos")]
            if let Some(name) = postscript_name_in(&bytes, font_index as usize)
                && let Some(mapped) = mapped_font_data_for_postscript_name(&name)
            {
                return Ok(mapped);
            }
            Ok(FontData {
                bytes: FontBytes::Owned(bytes.as_ref().clone()),
                index: font_index as usize,
            })
        }
        Handle::Path { path, font_index } => {
            let file = std::fs::File::open(&path).map_err(|_| FontError::NoFont)?;
            // SAFETY: mapping is UB if the file is truncated/rewritten while
            // mapped. Installed font files are replaced atomically (rename),
            // never mutated in place, so the mapping stays valid for its
            // lifetime — the same contract CoreText relies on.
            let map = unsafe { memmap2::Mmap::map(&file) }.map_err(|_| FontError::NoFont)?;
            Ok(FontData {
                bytes: FontBytes::Mapped(map),
                index: font_index as usize,
            })
        }
    }
}

/// Read the PostScript name of face `index` inside a font file/collection.
#[cfg(target_os = "macos")]
fn postscript_name_in(data: &[u8], index: usize) -> Option<String> {
    let font = FontRef::from_index(data, index)?;
    let name = font
        .localized_strings()
        .find_by_id(swash::StringId::PostScript, None)?;
    Some(name.chars().collect())
}

/// Resolve a PostScript name back to its installed font file via CoreText
/// (same normalized-descriptor lookup font-kit uses) and return the face
/// memory-mapped, locating the `.ttc` face index by PostScript name.
///
/// Returns `None` for faces without an on-disk file (e.g. downloadable
/// fonts); the caller keeps the in-memory bytes instead.
#[cfg(target_os = "macos")]
fn mapped_font_data_for_postscript_name(postscript_name: &str) -> Option<FontData> {
    let matched = matching_descriptors("NSFontNameAttribute", postscript_name)?;
    mapped_font_data_from_descriptor(&*matched.get(0)?)
}

/// Resolve the best normal-style face of a family via CoreText descriptors,
/// memory-mapped. Convenience wrapper over [`mapped_font_data_for_family_style`]
/// for the fallback stack (emoji/Nerd/CJK), which only ever wants the regular
/// cut.
#[cfg(target_os = "macos")]
fn mapped_font_data_for_family(family_name: &str) -> Option<FontData> {
    mapped_font_data_for_family_style(family_name, FontStyle::Regular)
}

/// Resolve the best face of `family_name` for `style` via CoreText descriptors,
/// memory-mapped — the no-slurp equivalent of font-kit's
/// `select_best_match` + [`resolve_required_style`] for the primary/style
/// faces. font-kit's CoreText source slurps every candidate file into the heap
/// during selection (Menlo.ttc is ~2 MB, re-slurped per style; Apple Color
/// Emoji ~190 MB), and the freed pages linger on libmalloc's death-row; walking
/// descriptors reads no font bytes until the one chosen file is mmapped.
///
/// Style resolution mirrors [`resolve_required_style`]'s trusted signals, in
/// order:
///
/// 1. **PostScript-name suffix** (e.g. "Menlo-BoldItalic") — the signal
///    `resolve_required_style` falls back to for families whose font-kit
///    `properties()` misreport (Menlo, Helvetica: font-kit 0.14.3's CoreText
///    backend reads every Menlo face as `Style::Italic`/weight 400). CoreText's
///    *descriptor* traits are reliable here — verified against Menlo's four
///    cuts — but the PostScript name is the strongest signal, so try it first
///    for a non-regular request.
/// 2. **CoreText descriptor traits** — italic bit + normalized weight. For
///    `Regular`, always resolvable: the non-italic face with weight nearest
///    regular (0.0), lighter on ties (matches font-kit's `Properties::new()`
///    pick — Hiragino W3 over W6). For a non-regular style with no PS-name
///    match, only a face whose traits genuinely carry the style qualifies;
///    otherwise `None`, so the caller synthesizes the style exactly as it does
///    when [`resolve_required_style`] returns `None`.
#[cfg(target_os = "macos")]
fn mapped_font_data_for_family_style(family_name: &str, style: FontStyle) -> Option<FontData> {
    use core_text::font_descriptor::{TraitAccessors, kCTFontItalicTrait};

    let matched = matching_descriptors("NSFontFamilyAttribute", family_name)?;

    // Tier 1: exact style match by PostScript-name suffix (skip for Regular —
    // "Menlo-Regular" would match but so would a bare "Menlo", and the trait
    // pass below picks the true regular cut deterministically).
    if !matches!(style, FontStyle::Regular) {
        for index in 0..matched.len() {
            let Some(descriptor) = matched.get(index) else {
                continue;
            };
            if postscript_name_matches_style(&descriptor.font_name(), style)
                && let Some(face) = mapped_font_data_from_descriptor(&descriptor)
            {
                return Some(face);
            }
        }
    }

    // Tier 2: descriptor traits. Rank non-italic/italic to match the request,
    // then weight fit (nearest regular for a non-bold request, heaviest for a
    // bold one), lighter on ties.
    let wants_bold = matches!(style, FontStyle::Bold | FontStyle::BoldItalic);
    let wants_italic = matches!(style, FontStyle::Italic | FontStyle::BoldItalic);
    let mut best: Option<((bool, f64, f64), isize)> = None;
    for index in 0..matched.len() {
        let Some(descriptor) = matched.get(index) else {
            continue;
        };
        let traits = descriptor.traits();
        let weight = traits.normalized_weight();
        let italic = traits.symbolic_traits() & kCTFontItalicTrait != 0;
        // For a non-regular request, reject faces that plainly lack the style
        // so an absent cut yields `None` (→ synthesis) rather than a wrong
        // face. A bold request needs meaningful weight (CoreText normalizes
        // Bold to ~0.4; ≥0.23 clears Medium without demanding Heavy).
        if !matches!(style, FontStyle::Regular) {
            if italic != wants_italic {
                continue;
            }
            if wants_bold && weight < 0.23 {
                continue;
            }
        }
        let italic_mismatch = italic != wants_italic;
        let weight_rank = if wants_bold { -weight } else { weight.abs() };
        let rank = (italic_mismatch, weight_rank, weight);
        if best.as_ref().is_none_or(|(current, _)| rank < *current) {
            best = Some((rank, index));
        }
    }
    let (_, index) = best?;
    mapped_font_data_from_descriptor(&*matched.get(index)?)
}

/// CoreText's normalized descriptors for `value` under the font attribute
/// `attribute_name` — carrying the file URL and PostScript name without
/// loading any font data (the whole point of this path).
#[cfg(target_os = "macos")]
fn matching_descriptors(
    attribute_name: &str,
    value: &str,
) -> Option<core_foundation::array::CFArray<core_text::font_descriptor::CTFontDescriptor>> {
    use core_foundation::array::CFArray;
    use core_foundation::base::TCFType;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::CFString;
    use core_text::{font_collection, font_descriptor};

    let attributes = CFDictionary::from_CFType_pairs(&[(
        CFString::new(attribute_name),
        CFString::new(value).as_CFType(),
    )]);
    let descriptor = font_descriptor::new_from_attributes(&attributes);
    let descriptors = CFArray::from_CFTypes(&[descriptor]);
    let collection = font_collection::new_from_descriptors(&descriptors);
    let matched = collection.get_descriptors()?;
    (!matched.is_empty()).then_some(matched)
}

#[cfg(target_os = "macos")]
fn mapped_font_data_from_descriptor(
    descriptor: &core_text::font_descriptor::CTFontDescriptor,
) -> Option<FontData> {
    let postscript_name = descriptor.font_name();
    let path = descriptor.font_path()?;
    let file = std::fs::File::open(&path).ok()?;
    // SAFETY: see the `Handle::Path` arm of `handle_to_data`.
    let map = unsafe { memmap2::Mmap::map(&file) }.ok()?;
    let index = face_index_for_postscript_name(&map, &postscript_name)?;
    Some(FontData {
        bytes: FontBytes::Mapped(map),
        index,
    })
}

/// Find the face inside `data` (a single font or a `.ttc` collection) whose
/// PostScript name is exactly `postscript_name`.
#[cfg(target_os = "macos")]
fn face_index_for_postscript_name(data: &[u8], postscript_name: &str) -> Option<usize> {
    let fonts = swash::FontDataRef::new(data)?;
    (0..fonts.len())
        .find(|&index| postscript_name_in(data, index).as_deref() == Some(postscript_name))
}

#[cfg(test)]
pub(crate) fn nerd_font_fallback_candidate_has_glyph(ch: char) -> bool {
    let source = SystemSource::new();
    nerd_font_fallback_family_names(&source)
        .into_iter()
        .any(|family_name| {
            let family = [FamilyName::Title(family_name)];
            let Ok(handle) = Source::select_best_match(&source, &family, &Properties::new()) else {
                return false;
            };
            let Some(data) = load_valid_handle(handle) else {
                return false;
            };
            let Ok(font) = data.font_ref() else {
                return false;
            };
            font.charmap().map(ch) != 0
        })
}

/// Scaled per-face metrics, in pixels, at a given pixels-per-em size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    /// Advance width of the monospace cell (pixels).
    pub cell_w: f32,
    /// Full line height of the cell (pixels).
    pub cell_h: f32,
    /// Ascent above the baseline (pixels).
    pub ascent: f32,
    /// Descent below the baseline (pixels, positive).
    pub descent: f32,
    /// Extra leading between lines (pixels).
    pub line_gap: f32,
    /// Underline position relative to the baseline (pixels, y-up).
    pub underline_position: f32,
    /// Underline stroke thickness (pixels).
    pub underline_thickness: f32,
}

impl Metrics {
    /// Compute metrics for `font` scaled to `px` pixels-per-em.
    ///
    /// `cell_w` is the advance of a representative monospace glyph ('M' if
    /// present, else the font's average advance width).
    pub fn compute(font: FontRef<'_>, px: f32) -> Self {
        let m = font.metrics(&[]).scale(px);

        let ascent = m.ascent;
        let descent = m.descent;
        let line_gap = m.leading;

        // Advance of 'M' at this size; fall back to average advance width.
        let gmetrics = font.glyph_metrics(&[]).scale(px);
        let m_gid = font.charmap().map('M');
        let cell_w = if m_gid != 0 {
            gmetrics.advance_width(m_gid)
        } else {
            0.0
        };
        let cell_w = if cell_w > 0.0 {
            cell_w
        } else {
            m.average_width
        };
        let cell_w = if cell_w > 0.0 {
            cell_w
        } else {
            // Last-resort estimate for degenerate faces.
            px * 0.6
        };

        let cell_w = quantize_cell_dimension(cell_w);
        let cell_h = quantize_cell_dimension(ascent + descent + line_gap);

        Metrics {
            cell_w,
            cell_h,
            ascent,
            descent,
            line_gap,
            underline_position: m.underline_offset,
            underline_thickness: if m.stroke_size > 0.0 {
                m.stroke_size
            } else {
                (px / 14.0).max(1.0)
            },
        }
    }
}

fn quantize_cell_dimension(value: f32) -> f32 {
    if value.is_finite() {
        value.round().max(1.0)
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FontConfig;

    #[test]
    fn font_stack_tracks_native_style_faces_separately() {
        let primary = FontData {
            bytes: vec![1, 2, 3].into(),
            index: 0,
        };
        let bold = FontData {
            bytes: vec![4, 5, 6].into(),
            index: 0,
        };
        let fallback = FontData {
            bytes: vec![7, 8, 9].into(),
            index: 0,
        };

        let stack = FontStack::new(primary, Some(bold), None, None, vec![fallback]);

        assert_eq!(stack.face_indices_for_style(FontStyle::Regular), &[0, 2]);
        assert_eq!(stack.face_indices_for_style(FontStyle::Bold), &[1, 2]);
        assert_eq!(stack.face_indices_for_style(FontStyle::Italic), &[0, 2]);
        assert!(stack.is_native_style_face(1, FontStyle::Bold));
        assert!(!stack.is_native_style_face(0, FontStyle::Bold));
    }

    #[test]
    fn duplicate_style_face_falls_back_to_regular_stack() {
        let primary = FontData {
            bytes: vec![1, 2, 3].into(),
            index: 0,
        };
        let same_as_primary = FontData {
            bytes: vec![1, 2, 3].into(),
            index: 0,
        };

        let stack = FontStack::new(primary, Some(same_as_primary), None, None, Vec::new());

        assert_eq!(stack.faces().len(), 1);
        assert_eq!(stack.face_indices_for_style(FontStyle::Bold), &[0]);
        assert!(!stack.is_native_style_face(0, FontStyle::Bold));
    }

    #[test]
    fn cell_metrics_are_quantized_to_whole_pixels() {
        assert_eq!(quantize_cell_dimension(7.49), 7.0);
        assert_eq!(quantize_cell_dimension(7.5), 8.0);
        assert_eq!(quantize_cell_dimension(0.25), 1.0);
        assert_eq!(quantize_cell_dimension(f32::NAN), 1.0);
    }

    #[test]
    fn nerd_font_family_detection_prioritizes_symbols_and_mono() {
        assert!(is_nerd_font_family_name("Symbols Nerd Font Mono"));
        assert!(is_nerd_font_family_name("CaskaydiaCove Nerd Font"));
        assert!(is_nerd_font_family_name("MesloLGS NF"));
        assert!(!is_nerd_font_family_name("Menlo"));

        assert!(
            nerd_font_family_priority("Symbols Nerd Font Mono")
                < nerd_font_family_priority("Hack Nerd Font Mono")
        );
        assert!(
            nerd_font_family_priority("Hack Nerd Font Mono")
                < nerd_font_family_priority("Hack Nerd Font Propo")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_default_monospace_candidate_is_menlo() {
        assert_eq!(default_monospace_family_names(), &["Menlo"]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn load_monospace_prefers_menlo_on_macos_when_available() {
        let source = SystemSource::new();
        let Ok(menlo_family) = source.select_family_by_name("Menlo") else {
            eprintln!("skipping: Menlo is not available in this environment");
            return;
        };
        let menlo_faces = menlo_family
            .fonts()
            .iter()
            .filter_map(|handle| load_valid_handle(handle.clone()))
            .collect::<Vec<_>>();
        if menlo_faces.is_empty() {
            eprintln!("skipping: Menlo has no loadable faces in this environment");
            return;
        }

        let actual = match load_monospace_from_source(&source) {
            Ok(actual) => actual,
            Err(e) => {
                eprintln!("skipping: no system monospace font available: {e}");
                return;
            }
        };

        assert!(
            menlo_faces
                .iter()
                .any(|candidate| font_data_matches(&actual, candidate)),
            "empty font config should prefer macOS' standard coding font, Menlo"
        );
    }

    /// AC-WP1-01 (REQ-EMOJI-1): given the resolved fallback stack from
    /// `load_font_stack`, an emoji codepoint resolves to the Apple Color
    /// Emoji face specifically — not merely *some* fallback face, and not a
    /// tofu/blank glyph.
    #[test]
    fn emoji_codepoint_resolves_to_apple_color_emoji_face() {
        let source = SystemSource::new();
        let Ok(emoji_family) = source.select_family_by_name(emoji_fallback_family_name()) else {
            eprintln!(
                "skipping: {} not installed in this environment",
                emoji_fallback_family_name()
            );
            return;
        };
        let Some(emoji_handle) = emoji_family.fonts().first().cloned() else {
            eprintln!(
                "skipping: {} family has no fonts",
                emoji_fallback_family_name()
            );
            return;
        };
        let emoji_data =
            handle_to_data(emoji_handle).expect("load Apple Color Emoji bytes directly");

        let stack = match load_font_stack(&FontConfig::default()) {
            Ok(stack) => stack,
            Err(e) => {
                eprintln!("skipping: no system monospace font available: {e}");
                return;
            }
        };

        // 😀 U+1F600 GRINNING FACE.
        let emoji_ch = '\u{1F600}';
        let resolved = stack.faces().iter().find(|face| {
            let Ok(font) = face.font_ref() else {
                return false;
            };
            font.charmap().map(emoji_ch) != 0
        });

        let resolved = resolved.expect(
            "an emoji codepoint must resolve to some face in the fallback stack, \
             not fall through to a tofu/blank glyph",
        );
        assert_eq!(
            resolved.bytes, emoji_data.bytes,
            "emoji codepoint should resolve to the Apple Color Emoji face specifically, \
             not an unrelated fallback face"
        );
    }

    /// `docs/specs/session-sidebar.md` NFR-5/AC-21 revision: every Nerd Font
    /// PUA codepoint the sidebar itself draws — the status dot (`nf-fa-circle`
    /// `U+F111`) and every `noa-app/src/sidebar.rs` `icon_glyph` variant —
    /// must resolve to a real glyph through [`load_font_stack`]'s fallback
    /// stack, independent of whether any Nerd Font family is installed
    /// system-wide. This is exactly the guarantee the embedded Symbols Nerd
    /// Font Mono face (see [`embedded_symbols_nerd_font_face`]) exists to
    /// provide, so this test must not skip on missing Nerd Fonts the way
    /// [`emoji_codepoint_resolves_to_apple_color_emoji_face`] skips on
    /// missing Apple Color Emoji.
    #[test]
    fn sidebar_icon_codepoints_resolve_without_installed_nerd_font() {
        let stack = match load_font_stack(&FontConfig::default()) {
            Ok(stack) => stack,
            Err(e) => {
                eprintln!("skipping: no system monospace font available: {e}");
                return;
            }
        };

        let sidebar_icon_codepoints = [
            '\u{F111}', // nf-fa-circle: sidebar status dot
            '\u{E5FF}', // nf-custom-folder: IconKind::Folder
            '\u{E7A8}', // nf-dev-rust: IconKind::Rust
            '\u{E718}', // nf-dev-nodejs_small: IconKind::Node
            '\u{E69A}', // nf-seti-terraform: IconKind::Terraform
            '\u{E627}', // nf-seti-go: IconKind::Go
            '\u{E606}', // nf-seti-python: IconKind::Python
            '\u{E702}', // nf-dev-git: IconKind::Git
        ];

        for ch in sidebar_icon_codepoints {
            let resolved = stack
                .face_indices_for_style(FontStyle::Regular)
                .iter()
                .find_map(|&face_index| {
                    let font = stack.faces()[face_index].font_ref().ok()?;
                    let glyph_id = font.charmap().map(ch);
                    (glyph_id != 0).then_some(glyph_id)
                });
            assert!(
                resolved.is_some(),
                "U+{:04X} must resolve to a real (non-notdef) glyph through the fallback \
                 stack even with no Nerd Font installed",
                ch as u32
            );
        }
    }

    /// The embedded Symbols Nerd Font Mono face must classify as an icon
    /// fallback face (`FontStack::is_icon_fallback_face`) so it inherits the
    /// cell-constrained icon sizing/styling parity from commit fc50f8b
    /// ("align fallback styling and glyph sizing with Ghostty"), the same as
    /// an installed Nerd Font would.
    #[test]
    fn embedded_symbols_font_is_classified_icon_fallback_face() {
        let embedded = embedded_symbols_nerd_font_face().expect("embedded font must parse");
        let dummy_primary = FontData {
            bytes: vec![1, 2, 3].into(),
            index: 0,
        };
        let stack = FontStack::new(dummy_primary, None, None, None, vec![embedded]);

        // Fallback faces are appended after the primary (index 0), so the
        // single fallback here lands at index 1.
        assert!(stack.is_icon_fallback_face(1));
    }

    /// System fonts must end up memory-mapped, not heap-copied: font-kit's
    /// CoreText source returns `Handle::Memory` with the whole file slurped,
    /// and keeping that copy per FontGrid cost hundreds of MB of dirty
    /// footprint (Apple Color Emoji alone is ~190 MB). The one exception is
    /// the embedded Symbols Nerd Font Mono fallback (`FontBytes::Static`),
    /// which is not a system font at all — it is compiled into the binary,
    /// so it is already resident without a heap copy or an `mmap` call.
    #[test]
    #[cfg(target_os = "macos")]
    fn system_font_faces_are_memory_mapped() {
        let stack = match load_font_stack(&FontConfig::default()) {
            Ok(stack) => stack,
            Err(e) => {
                eprintln!("skipping: no system monospace font available: {e}");
                return;
            }
        };
        for face in stack.faces() {
            assert!(
                matches!(face.bytes, FontBytes::Mapped(_) | FontBytes::Static(_)),
                "system font face should be memory-mapped (or the compiled-in embedded \
                 fallback), got {:?}",
                face.bytes
            );
            assert!(face.font_ref().is_ok(), "mapped face must stay parseable");
        }
    }

    /// Primary/style resolution must take the no-slurp CoreText→mmap path
    /// ([`mapped_font_data_for_family_style`]) and land on the *same*
    /// PostScript name font-kit's slurping selector + [`resolve_required_style`]
    /// would pick — for every style. This is the parity contract behind
    /// rerouting the primary/style faces off font-kit: bytes stay mapped (no
    /// death-row heap slurp) and the selected face is unchanged. Menlo is the
    /// macOS default coding face and the family whose font-kit `properties()`
    /// misreport, so it is the critical case.
    #[test]
    #[cfg(target_os = "macos")]
    fn menlo_style_faces_resolve_via_mmap_matching_fontkit() {
        let source = SystemSource::new();
        if source.select_family_by_name("Menlo").is_err() {
            eprintln!("skipping: Menlo not available in this environment");
            return;
        }
        for style in [
            FontStyle::Regular,
            FontStyle::Bold,
            FontStyle::Italic,
            FontStyle::BoldItalic,
        ] {
            let mapped = mapped_font_data_for_family_style("Menlo", style)
                .expect("Menlo style must resolve via the mmap path");
            assert!(
                matches!(mapped.bytes, FontBytes::Mapped(_)),
                "{style:?}: primary/style face must be memory-mapped, got {:?}",
                mapped.bytes
            );
            let mapped_ps = postscript_name_in(&mapped.bytes, mapped.index);

            // font-kit reference selection, mirroring the default-config path
            // (load_default_monospace_family passes Some(style) for all styles).
            let props = properties_for_style(style);
            let fontkit_ps = select_title_best_match(&source, "Menlo", &props)
                .ok()
                .and_then(|handle| resolve_required_style(&source, handle, style))
                .and_then(load_valid_handle)
                .and_then(|data| postscript_name_in(&data.bytes, data.index));

            assert_eq!(
                mapped_ps, fontkit_ps,
                "{style:?}: mmap path must select the same PostScript name as font-kit"
            );
        }
    }

    /// Rendering parity backup for the primary/style reroute: glyphs
    /// rasterized from the mmap-selected face must be **byte-identical** to
    /// glyphs rasterized from the font-kit-selected face, for every style and
    /// for both ordinary letters and box-drawing glyphs. Stronger than a screen
    /// capture (no compositor/P3 noise): the rasterizer is a pure function of
    /// (face bytes, glyph id, size), so identical output proves the reroute
    /// changed nothing the renderer can see.
    #[test]
    #[cfg(target_os = "macos")]
    fn menlo_style_rasterization_is_identical_via_mmap_and_fontkit() {
        use crate::raster::{GlyphSynthesis, rasterize_with_variations};
        use swash::scale::ScaleContext;

        let source = SystemSource::new();
        if source.select_family_by_name("Menlo").is_err() {
            eprintln!("skipping: Menlo not available in this environment");
            return;
        }
        let mut ctx = ScaleContext::new();
        let sample_chars = ['A', 'g', '@', '│', '┼', '╳'];

        for style in [
            FontStyle::Regular,
            FontStyle::Bold,
            FontStyle::Italic,
            FontStyle::BoldItalic,
        ] {
            let mmap_face = mapped_font_data_for_family_style("Menlo", style)
                .expect("mmap path must resolve Menlo style");
            let props = properties_for_style(style);
            let fontkit_face = select_title_best_match(&source, "Menlo", &props)
                .ok()
                .and_then(|handle| resolve_required_style(&source, handle, style))
                .and_then(load_valid_handle)
                .expect("font-kit path must resolve Menlo style");

            let mmap_font = mmap_face.font_ref().expect("mmap face parses");
            let fontkit_font = fontkit_face.font_ref().expect("font-kit face parses");

            for ch in sample_chars {
                let mmap_gid = mmap_font.charmap().map(ch);
                let fontkit_gid = fontkit_font.charmap().map(ch);
                assert_eq!(
                    mmap_gid, fontkit_gid,
                    "{style:?} '{ch}': glyph id must match across selection paths"
                );
                let syn = GlyphSynthesis::default();
                let a =
                    rasterize_with_variations(&mut ctx, mmap_font, mmap_gid, 24.0, &[], syn, None);
                let b = rasterize_with_variations(
                    &mut ctx,
                    fontkit_font,
                    fontkit_gid,
                    24.0,
                    &[],
                    syn,
                    None,
                );
                assert_eq!(
                    (a.width, a.height, a.bearing_x, a.bearing_y, a.bitmap),
                    (b.width, b.height, b.bearing_x, b.bearing_y, b.bitmap),
                    "{style:?} '{ch}': rasterized coverage must be byte-identical"
                );
            }
        }
    }

    /// The mmap re-resolution must map the same face the handle described:
    /// same bytes, correct `.ttc` index (PostScript names must match).
    #[test]
    #[cfg(target_os = "macos")]
    fn mapped_face_matches_handle_memory_bytes() {
        let source = SystemSource::new();
        let Ok(handle) = source.select_by_postscript_name("AppleColorEmoji") else {
            eprintln!("skipping: Apple Color Emoji not installed");
            return;
        };
        let (memory_bytes, memory_index) = match &handle {
            Handle::Memory { bytes, font_index } => (bytes.clone(), *font_index as usize),
            Handle::Path { .. } => {
                eprintln!("skipping: font-kit returned a path handle; nothing to re-resolve");
                return;
            }
        };
        let data = handle_to_data(handle).expect("load Apple Color Emoji");
        assert!(matches!(data.bytes, FontBytes::Mapped(_)));
        assert_eq!(
            postscript_name_in(&data.bytes, data.index),
            postscript_name_in(&memory_bytes, memory_index),
            "mapped face must be the exact face the handle selected"
        );
    }

    /// Acceptance (a): no CJK font is resolved/mapped at startup. The default
    /// stack is primary (Menlo on macOS) + emoji + Nerd + embedded symbols —
    /// none of which carry kanji — so a common kanji must NOT be covered by
    /// any face already in the stack. It is only resolved later, lazily, via
    /// [`cjk_fallback_face_for`]. (Observable externally as vmmap showing no
    /// Hiragino/AppleGothic mapped-file regions before CJK is drawn.)
    #[test]
    fn startup_stack_does_not_cover_cjk() {
        let stack = match load_font_stack(&FontConfig::default()) {
            Ok(stack) => stack,
            Err(e) => {
                eprintln!("skipping: no system monospace font available: {e}");
                return;
            }
        };
        // U+65E5 '日' — a common kanji absent from Menlo/emoji/Nerd/symbols.
        let covered = stack.faces().iter().any(|face| {
            face.font_ref()
                .ok()
                .is_some_and(|font| font.charmap().map('日') != 0)
        });
        assert!(
            !covered,
            "the startup font stack must not cover kanji — CJK fonts load lazily, not eagerly"
        );
    }

    /// The lazy CJK resolver must (1) return a face that actually covers the
    /// codepoint and (2) pick the same face the old eager priority list would
    /// have — PostScript names first, then families, each in list order — so
    /// Japanese text renders with exactly the same fonts as before.
    #[test]
    fn cjk_fallback_resolves_in_eager_priority_order() {
        let Some(face) = cjk_fallback_face_for('日') else {
            eprintln!("skipping: no curated CJK font installed to resolve U+65E5");
            return;
        };
        assert!(
            face.font_ref()
                .expect("resolved face parses")
                .charmap()
                .map('日')
                != 0,
            "cjk_fallback_face_for must return a face that covers the codepoint"
        );

        // Independently walk the same priority list; the winner must match.
        let source = SystemSource::new();
        let expected = cjk_fallback_postscript_names()
            .iter()
            .find_map(|name| face_covering(postscript_fallback_face(&source, name), '日'))
            .or_else(|| {
                cjk_fallback_family_names()
                    .iter()
                    .find_map(|name| face_covering(family_fallback_face(&source, name), '日'))
            })
            .expect("the same priority walk must also find a covering face");

        #[cfg(target_os = "macos")]
        assert_eq!(
            postscript_name_in(&face.bytes, face.index),
            postscript_name_in(&expected.bytes, expected.index),
            "lazy CJK resolution must select the same face the eager priority list would"
        );
        #[cfg(not(target_os = "macos"))]
        assert_eq!(
            (face.bytes, face.index),
            (expected.bytes, expected.index),
            "lazy CJK resolution must select the same face the eager priority list would"
        );
    }

    /// `+list-fonts` relies on this shape: at least one family exists on a
    /// system with fonts, and the list is strictly sorted (which also proves
    /// deduplication).
    #[test]
    fn list_families_is_non_empty_sorted_and_deduped() {
        let families = list_families().expect("system font source should enumerate");

        assert!(!families.is_empty());
        assert!(
            families.windows(2).all(|pair| pair[0] < pair[1]),
            "family list must be strictly sorted (sorted + deduped)"
        );
    }
}
