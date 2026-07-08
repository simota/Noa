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
}

impl std::ops::Deref for FontBytes {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match self {
            FontBytes::Owned(bytes) => bytes,
            FontBytes::Mapped(map) => map,
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
        match style {
            FontStyle::Regular => true,
            FontStyle::Bold => self.native_bold_face == Some(face_index),
            FontStyle::Italic => self.native_italic_face == Some(face_index),
            FontStyle::BoldItalic => self.native_bold_italic_face == Some(face_index),
        }
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
pub fn load_monospace() -> Result<FontData, FontError> {
    let source = SystemSource::new();
    load_monospace_from_source(&source)
}

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
/// platform default coding font (see [`load_monospace`]) when
/// `font_cfg.families` is empty or none of the configured families resolve.
///
/// Configured regular/bold/italic families are resolved with font-kit's CSS
/// matcher. If a native style face is unavailable, the style stack falls back
/// to the regular primary and rasterization may synthesize the missing style.
pub fn load_font_stack(font_cfg: &FontConfig) -> Result<FontStack, FontError> {
    let source = SystemSource::new();
    let primary = match load_configured_primary(&source, font_cfg) {
        Some(primary) => primary,
        None => load_monospace()?,
    };
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

    for postscript_name in cjk_fallback_postscript_names() {
        push_some_face(
            &mut fallbacks,
            postscript_fallback_face(&source, postscript_name),
        );
    }
    for family_name in cjk_fallback_family_names() {
        push_some_face(&mut fallbacks, family_fallback_face(&source, family_name));
    }

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
    if let Some(style) = required_style
        && !handle_supports_style(&handle, style)
    {
        return None;
    }
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

fn load_title_family(
    source: &SystemSource,
    family_name: &str,
    properties: &Properties,
    required_style: Option<FontStyle>,
) -> Option<FontData> {
    let handle = select_title_best_match(source, family_name, properties).ok()?;
    if let Some(style) = required_style
        && !handle_supports_style(&handle, style)
    {
        return None;
    }
    load_valid_handle(handle)
}

fn load_first_matching_family(
    source: &SystemSource,
    families: &[String],
    properties: &Properties,
    required_style: Option<FontStyle>,
) -> Option<FontData> {
    families.iter().find_map(|family_name| {
        let handle = select_title_best_match(source, family_name, properties).ok()?;
        if let Some(style) = required_style
            && !handle_supports_style(&handle, style)
        {
            return None;
        }
        load_valid_handle(handle)
    })
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
/// memory-mapped. Skips italics and takes the weight closest to regular
/// (CoreText normalized weight 0.0), lighter on ties — the face font-kit's
/// CSS matcher picks for `Properties::new()` in practice (e.g. Hiragino W3
/// over W6, "Regular" over "Bold").
#[cfg(target_os = "macos")]
fn mapped_font_data_for_family(family_name: &str) -> Option<FontData> {
    use core_text::font_descriptor::{TraitAccessors, kCTFontItalicTrait};

    let matched = matching_descriptors("NSFontFamilyAttribute", family_name)?;
    let mut best: Option<((bool, f64, f64), isize)> = None;
    for index in 0..matched.len() {
        let Some(descriptor) = matched.get(index) else {
            continue;
        };
        let traits = descriptor.traits();
        let weight = traits.normalized_weight();
        let italic = traits.symbolic_traits() & kCTFontItalicTrait != 0;
        let rank = (italic, weight.abs(), weight);
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
    (0..fonts.len()).find(|&index| postscript_name_in(data, index).as_deref() == Some(postscript_name))
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

    /// System fonts must end up memory-mapped, not heap-copied: font-kit's
    /// CoreText source returns `Handle::Memory` with the whole file slurped,
    /// and keeping that copy per FontGrid cost hundreds of MB of dirty
    /// footprint (Apple Color Emoji alone is ~190 MB).
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
                matches!(face.bytes, FontBytes::Mapped(_)),
                "system font face should be memory-mapped, got {:?}",
                face.bytes
            );
            assert!(face.font_ref().is_ok(), "mapped face must stay parseable");
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
