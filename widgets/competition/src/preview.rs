//! Self-contained previews for the comparison viewer.
//!
//! `compare.html` is opened as a local file and embeds each entry in an
//! `<iframe sandbox="allow-scripts">`. That frame has an opaque origin, and
//! Chrome refuses every relative `file:` subresource it asks for, so an
//! entry's own `index.html` shows up without its stylesheet, script and
//! images. Instead of asking every contestant to hand-build an inlined copy,
//! the snapshot step writes one: [`write`] reads the snapshot's root
//! `index.html` and inlines the local stylesheets, scripts, images and fonts it
//! references, so the viewer can frame the entry exactly as submitted.

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

/// Directory (inside a round snapshot) holding the generated preview. It
/// starts with a dot, so the snapshot copy never lists it as an artifact.
pub const PREVIEW_DIR: &str = ".preview";
/// The entry page the preview is generated from.
pub const ENTRY_PAGE: &str = "index.html";

/// Largest single file inlined into the preview; bigger ones stay linked.
const MAX_INLINE_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// The generated preview path for a snapshot directory.
pub fn preview_path(snapshot_dir: &Path) -> PathBuf {
    snapshot_dir.join(PREVIEW_DIR).join(ENTRY_PAGE)
}

/// A written preview and the local references it could not inline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    pub page: PathBuf,
    /// One line per reference left as is: missing file, outside the entry
    /// directory, or over the inline size limit. The viewer cannot load them.
    pub issues: Vec<String>,
}

/// What the preview inlines, for briefs and manifests.
pub const INLINE_RULES: &str = "Inlined from the root `index.html`: `<link rel=\"stylesheet\">`, `<script src>` (in place; a deferred, async or module script keeps those attributes with its code as a `data:` URI), leading `@import`s in stylesheets, `src`/`href`/`poster` resources such as images, icons and fonts, and `url(...)` in stylesheets and `style` attributes, each when it is a relative path to a file of at most 8 MiB inside the entry directory. Not inlined (relative ones are pointed back at the entry with `../`): `srcset`, URLs built by scripts, `fetch`/XHR, other pages (links stay links), and absolute or remote URLs.";

/// Writes the inlined preview of `snapshot_dir/index.html`. Returns `None`
/// when the snapshot has no root `index.html`.
pub fn write(snapshot_dir: &Path) -> io::Result<Option<Preview>> {
    let page = snapshot_dir.join(ENTRY_PAGE);
    if !page.is_file() {
        return Ok(None);
    }
    let html = fs::read_to_string(&page)?;
    let ctx = Ctx {
        root: snapshot_dir,
        issues: Default::default(),
    };
    let inlined = inline_html(&html, &ctx, Path::new(""));
    let out = preview_path(snapshot_dir);
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&out, inlined)?;
    Ok(Some(Preview {
        page: out,
        issues: ctx.issues.into_inner(),
    }))
}

/// Resolves `reference` (as written in a file under `base`, relative to
/// `root`) to a file inside `root`. Remote, absolute, data and fragment
/// references, and any path that would leave `root`, resolve to `None`.
fn resolve(ctx: &Ctx, base: &Path, reference: &str) -> Option<PathBuf> {
    let reference = reference.trim();
    let path_part = reference.split(['?', '#']).next().unwrap_or_default();
    if path_part.is_empty()
        || path_part.starts_with('/')
        || path_part.starts_with("//")
        || path_part.contains(':')
    {
        return None;
    }
    let Some(decoded) = percent_decode(path_part) else {
        ctx.note(format!("{reference}: not a valid path"));
        return None;
    };
    let mut rel = PathBuf::new();
    for component in base.join(&decoded).components() {
        match component {
            Component::Normal(part) => rel.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                if !rel.pop() {
                    ctx.note(format!("{reference}: points outside the entry directory"));
                    return None;
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    let full = ctx.root.join(&rel);
    match fs::symlink_metadata(&full) {
        Ok(meta) if meta.is_file() && meta.len() <= MAX_INLINE_FILE_BYTES => Some(full),
        Ok(meta) if meta.is_file() => {
            ctx.note(format!(
                "{reference}: {} bytes, over the {MAX_INLINE_FILE_BYTES}-byte inline limit (left as a link, which the viewer cannot load)",
                meta.len()
            ));
            None
        }
        _ => {
            ctx.note(format!("{reference}: file not found"));
            None
        }
    }
}

/// What inlining works against, and what it could not inline.
struct Ctx<'a> {
    root: &'a Path,
    issues: std::cell::RefCell<Vec<String>>,
}

impl Ctx<'_> {
    fn note(&self, issue: String) {
        let mut issues = self.issues.borrow_mut();
        if !issues.contains(&issue) {
            issues.push(issue);
        }
    }
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn mime_for(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        _ => "application/octet-stream",
    }
}

/// Pages are framed or linked, never inlined as data.
fn is_page(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("html") || e.eq_ignore_ascii_case("htm"))
}

fn data_uri(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    Some(format!("data:{};base64,{}", mime_for(path), base64(&bytes)))
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// How deep `@import` chains are followed before the rest stay as they are.
const MAX_IMPORT_DEPTH: usize = 8;

/// Points a reference the copy leaves as is back at the entry: the copy sits
/// one directory down, in `.preview/`. `base` is the directory (relative to
/// the entry) of the file the reference was written in. Fragment-only,
/// absolute, remote and data references stay as they are.
fn rebase(reference: &str, base: &Path) -> Option<String> {
    let trimmed = reference.trim();
    let path_part = trimmed.split(['?', '#']).next().unwrap_or_default();
    if trimmed.is_empty()
        || trimmed.starts_with('#')
        || trimmed.starts_with('/')
        || path_part.contains(':')
    {
        return None;
    }
    let mut rebased = String::from("../");
    for part in base.components() {
        rebased.push_str(&part.as_os_str().to_string_lossy());
        rebased.push('/');
    }
    rebased.push_str(trimmed);
    Some(rebased)
}

/// Inlines a stylesheet: the leading `@import`s (after any comments and
/// `@charset`) become the imported sheets, inlined in turn and wrapped in
/// `@media` when the import names one, and every local `url(...)` becomes a
/// data URI. Imports that cannot be inlined stay imports, moved ahead of all
/// inlined rules: a browser ignores an `@import` that follows a rule. `base`
/// is the stylesheet's directory relative to the entry root.
fn inline_css(css: &str, ctx: &Ctx, base: &Path) -> String {
    let (imports, body) = inline_css_at(css, ctx, base, 0);
    imports.concat() + &body
}

/// The kept `@import` statements and the inlined rest of a stylesheet.
fn inline_css_at(css: &str, ctx: &Ctx, base: &Path, depth: usize) -> (Vec<String>, String) {
    let mut kept = Vec::new();
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    loop {
        let trimmed = rest.trim_start();
        if trimmed.starts_with("/*") {
            let end = trimmed.find("*/").map_or(trimmed.len(), |e| e + 2);
            rest = &trimmed[end..];
            continue;
        }
        let at_rule = |name: &str| {
            trimmed
                .get(..name.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(name))
        };
        if at_rule("@charset") {
            // Meaningless inside an HTML page; dropped.
            rest = statement_end(trimmed).map_or("", |end| &trimmed[end + 1..]);
            continue;
        }
        if !at_rule("@import") {
            break;
        }
        let Some(end) = statement_end(trimmed) else {
            break;
        };
        let (target, media) = split_import(&trimmed[7..end]);
        rest = &trimmed[end + 1..];
        let inlined = (depth < MAX_IMPORT_DEPTH)
            .then(|| resolve(ctx, base, &target))
            .flatten()
            .and_then(|file| {
                let sheet = fs::read_to_string(&file).ok()?;
                let sheet_base = file
                    .strip_prefix(ctx.root)
                    .ok()
                    .and_then(Path::parent)
                    .map(Path::to_path_buf)
                    .unwrap_or_default();
                Some(inline_css_at(&sheet, ctx, &sheet_base, depth + 1))
            });
        match inlined {
            Some((nested, sheet)) => {
                for import in nested {
                    // A nested import keeps the outer import's media.
                    kept.push(if media.is_empty() {
                        import
                    } else {
                        import.replacen(") ", &format!(") {media} "), 1)
                    });
                }
                if media.is_empty() {
                    out.push_str(&sheet);
                } else {
                    let _ = write!(out, "@media {media} {{{sheet}}}");
                }
            }
            None => {
                let target = rebase(&target, base).unwrap_or(target);
                kept.push(format!("@import url(\"{target}\") {media};\n"));
            }
        }
    }
    while let Some(at) = rest.find("url(") {
        let (head, tail) = rest.split_at(at + 4);
        out.push_str(head);
        let Some(close) = tail.find(')') else {
            rest = tail;
            break;
        };
        let raw = &tail[..close];
        let target = raw.trim().trim_matches(['"', '\'']);
        match resolve(ctx, base, target).and_then(|p| data_uri(&p)) {
            Some(uri) => {
                out.push('"');
                out.push_str(&uri);
                out.push_str(&fragment(target));
                out.push('"');
            }
            None => match rebase(target, base) {
                Some(rebased) => {
                    out.push('"');
                    out.push_str(&rebased);
                    out.push('"');
                }
                None => out.push_str(raw),
            },
        }
        rest = &tail[close..];
    }
    out.push_str(rest);
    (kept, out)
}

/// The byte index of the `;` ending the CSS statement `text` starts with,
/// skipping `;` inside quotes and parentheses (`url(...;...)`).
fn statement_end(text: &str) -> Option<usize> {
    let mut quote = None;
    let mut depth = 0usize;
    for (i, c) in text.char_indices() {
        match (quote, c) {
            (Some(q), _) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '(') => depth += 1,
            (None, ')') => depth = depth.saturating_sub(1),
            (None, ';') if depth == 0 => return Some(i),
            _ => {}
        }
    }
    None
}

/// The target and media list of an `@import` statement's body.
fn split_import(statement: &str) -> (String, String) {
    let s = statement.trim();
    let unquote = |t: &str| -> (String, usize) {
        match t.chars().next().filter(|c| *c == '"' || *c == '\'') {
            Some(q) => {
                let close = t[1..].find(q).map_or(t.len(), |c| c + 1);
                (t[1..close.min(t.len())].to_owned(), close + 1)
            }
            None => {
                let close = t.find(')').unwrap_or(t.len());
                (t[..close].trim().to_owned(), close)
            }
        }
    };
    let (target, after) = if let Some(inner) = s.strip_prefix("url(") {
        let inner = inner.trim_start();
        let (target, used) = unquote(inner);
        let after = inner.get(used..).unwrap_or_default();
        let after = after.find(')').map_or(after, |c| &after[c + 1..]);
        (target, after)
    } else {
        let (target, used) = unquote(s);
        (target, s.get(used..).unwrap_or_default())
    };
    (target, after.trim().to_owned())
}

/// `srcset` with each candidate URL rebased. Candidates are split the way
/// browsers do: a URL runs to whitespace (so a `data:` URI keeps its commas),
/// then descriptors run to the next comma.
fn rebase_srcset(srcset: &str, base: &Path) -> String {
    let mut out = String::with_capacity(srcset.len());
    let mut rest = srcset;
    loop {
        let start = rest.len()
            - rest
                .trim_start_matches(|c: char| c.is_whitespace() || c == ',')
                .len();
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        if rest.is_empty() {
            break;
        }
        let url_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let mut url = &rest[..url_end];
        let mut after = &rest[url_end..];
        // `a.png,b.png`: a URL that ends in commas ends the candidate there.
        let trailing = url.len() - url.trim_end_matches(',').len();
        if trailing > 0 && !url.starts_with("data:") {
            after = &rest[url_end - trailing..];
            url = &url[..url.len() - trailing];
        }
        match rebase(url, base) {
            Some(rebased) => out.push_str(&rebased),
            None => out.push_str(url),
        }
        let desc_end = after.find(',').unwrap_or(after.len());
        out.push_str(&after[..desc_end]);
        rest = &after[desc_end..];
    }
    out
}

/// The `#fragment` of a reference, kept on its data URI (an SVG view).
fn fragment(reference: &str) -> String {
    reference
        .find('#')
        .map(|at| reference[at..].trim().to_owned())
        .unwrap_or_default()
}

/// One attribute of a start tag.
struct Attr {
    key: String,
    /// The decoded value; `None` for a bare attribute like `defer`.
    value: Option<String>,
    /// The attribute exactly as written, rendered back while it is unchanged
    /// so character references and quoting survive untouched.
    raw: Option<String>,
}

/// One parsed start tag: its name and attributes in source order.
struct Tag {
    name: String,
    attrs: Vec<Attr>,
    /// What stood between the last attribute and `>`, such as the ` /` of a
    /// self-closing SVG element, kept so foreign content still nests right.
    tail: String,
}

impl Tag {
    fn get(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|a| a.key.eq_ignore_ascii_case(name))
            .and_then(|a| a.value.as_deref())
    }

    fn has(&self, name: &str) -> bool {
        self.attrs.iter().any(|a| a.key.eq_ignore_ascii_case(name))
    }

    fn remove(&mut self, names: &[&str]) {
        self.attrs
            .retain(|a| !names.iter().any(|n| a.key.eq_ignore_ascii_case(n)));
    }

    fn set(&mut self, name: &str, value: String) {
        if let Some(slot) = self
            .attrs
            .iter_mut()
            .find(|a| a.key.eq_ignore_ascii_case(name))
        {
            slot.value = Some(value);
            slot.raw = None;
        }
    }

    fn render(&self) -> String {
        let mut out = format!("<{}", self.name);
        for attr in &self.attrs {
            out.push(' ');
            match (&attr.raw, &attr.value) {
                (Some(raw), _) => out.push_str(raw),
                (None, Some(value)) => {
                    out.push_str(&attr.key);
                    out.push_str("=\"");
                    out.push_str(&value.replace('&', "&amp;").replace('"', "&quot;"));
                    out.push('"');
                }
                (None, None) => out.push_str(&attr.key),
            }
        }
        out.push_str(&self.tail);
        out.push('>');
        out
    }
}

/// Parses the start tag at the beginning of `text` (which starts with `<`).
/// Returns the tag and its byte length, or `None` for anything else
/// (comments, end tags, doctype, a stray `<`).
fn parse_tag(text: &str) -> Option<(Tag, usize)> {
    let bytes = text.as_bytes();
    let mut i = 1;
    let start = i;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-') {
        i += 1;
    }
    if i == start || !bytes[start].is_ascii_alphabetic() {
        return None;
    }
    let name = text[start..i].to_owned();
    let mut attrs = Vec::new();
    loop {
        let gap_start = i;
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            i += 1;
        }
        if i >= bytes.len() {
            return None;
        }
        if bytes[i] == b'>' {
            let tail = text[gap_start..i].to_owned();
            return Some((Tag { name, attrs, tail }, i + 1));
        }
        let key_start = i;
        while i < bytes.len()
            && !bytes[i].is_ascii_whitespace()
            && !matches!(bytes[i], b'=' | b'>' | b'/')
        {
            i += 1;
        }
        let key = text[key_start..i].to_owned();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            let value = if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
                let quote = bytes[i];
                let value_start = i + 1;
                let end = text[value_start..].find(quote as char)? + value_start;
                i = end + 1;
                &text[value_start..end]
            } else {
                let value_start = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' {
                    i += 1;
                }
                &text[value_start..i]
            };
            attrs.push(Attr {
                value: Some(decode_entities(value)),
                raw: Some(text[key_start..i].to_owned()),
                key,
            });
        } else if !key.is_empty() {
            attrs.push(Attr {
                raw: Some(key.clone()),
                key,
                value: None,
            });
        }
    }
}

fn decode_entities(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Keeps inlined script or style text from closing its element early.
fn escape_raw_text(text: &str, element: &str) -> String {
    let close = format!("</{element}");
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = find_ascii_ci(rest, &close) {
        out.push_str(&rest[..at]);
        out.push_str("<\\/");
        out.push_str(&rest[at + 2..at + close.len()]);
        rest = &rest[at + close.len()..];
    }
    out.push_str(rest);
    out
}

fn find_ascii_ci(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .as_bytes()
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
}

/// Attributes whose value is a single resource URL to inline as a data URI.
const URL_ATTRS: [&str; 3] = ["src", "poster", "href"];

/// Inlines the local resources an HTML document references. `base` is the
/// document's directory relative to `root`.
fn inline_html(html: &str, ctx: &Ctx, base: &Path) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(at) = rest.find('<') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        if rest.starts_with("<!--") {
            let end = rest.find("-->").map_or(rest.len(), |e| e + 3);
            out.push_str(&rest[..end]);
            rest = &rest[end..];
            continue;
        }
        let Some((mut tag, len)) = parse_tag(rest) else {
            out.push('<');
            rest = &rest[1..];
            continue;
        };
        rest = &rest[len..];
        let name = tag.name.to_ascii_lowercase();
        // Raw-text elements: copy their body verbatim (or inline it).
        if name == "script" || name == "style" {
            let close = find_ascii_ci(rest, &format!("</{name}")).unwrap_or(rest.len());
            let body = &rest[..close];
            rest = &rest[close..];
            // A local script stays where it is, so its timing and its order
            // against every other script hold. A deferred, async or module
            // one keeps those attributes with its code as a `data:` URI
            // (inline code would run at once); a plain one is inlined.
            if name == "script"
                && let Some(src) = tag.get("src").map(str::to_owned)
            {
                let local = resolve(ctx, base, &src)
                    .and_then(|file| Some((fs::read_to_string(&file).ok()?, file)));
                match local {
                    Some((code, file)) => {
                        let late = tag.has("defer")
                            || tag.has("async")
                            || tag.get("type") == Some("module");
                        if late {
                            let Some(uri) = data_uri(&file) else {
                                continue;
                            };
                            tag.set("src", uri.replacen(mime_for(&file), "text/javascript", 1));
                            tag.remove(&["integrity"]);
                            out.push_str(&tag.render());
                            out.push_str(body);
                        } else {
                            tag.remove(&["src", "integrity", "crossorigin"]);
                            out.push_str(&tag.render());
                            out.push_str(&escape_raw_text(&code, "script"));
                        }
                    }
                    None => {
                        if let Some(rebased) = rebase(&src, base) {
                            tag.set("src", rebased);
                        }
                        out.push_str(&tag.render());
                        out.push_str(body);
                    }
                }
                continue;
            }
            out.push_str(&tag.render());
            if name == "style" {
                out.push_str(&inline_css(body, ctx, base));
            } else {
                out.push_str(body);
            }
            continue;
        }
        if name == "link" {
            let rel = tag.get("rel").unwrap_or_default().to_ascii_lowercase();
            if rel.split_whitespace().any(|r| r == "stylesheet")
                && let Some(file) = tag.get("href").and_then(|href| resolve(ctx, base, href))
                && let Ok(css) = fs::read_to_string(&file)
            {
                let css_base = file
                    .strip_prefix(ctx.root)
                    .ok()
                    .and_then(Path::parent)
                    .map(Path::to_path_buf)
                    .unwrap_or_default();
                let media = tag
                    .get("media")
                    .map(|m| format!(" media=\"{}\"", m.replace('"', "&quot;")))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "<style{media}>{}</style>",
                    escape_raw_text(&inline_css(&css, ctx, &css_base), "style")
                ));
                continue;
            }
        }
        // Links to other pages stay links; resource URLs become data URIs.
        // Whatever stays a reference is pointed back at the entry, since the
        // copy lives one directory down.
        let navigates = name == "a" || name == "area" || name == "form";
        for attr in URL_ATTRS.iter().chain(&["action"]) {
            let Some(value) = tag.get(attr).map(str::to_owned) else {
                continue;
            };
            let inlined = (!navigates && *attr != "action")
                .then(|| resolve(ctx, base, &value))
                .flatten()
                .filter(|p| !is_page(p))
                .and_then(|p| data_uri(&p))
                .map(|uri| uri + &fragment(&value));
            if let Some(uri) = inlined.or_else(|| rebase(&value, base)) {
                tag.set(attr, uri);
            }
        }
        if let Some(srcset) = tag.get("srcset").map(str::to_owned) {
            let rebased = rebase_srcset(&srcset, base);
            if rebased != srcset {
                tag.set("srcset", rebased);
            }
        }
        if let Some(style) = tag.get("style").map(str::to_owned) {
            let inlined = inline_css(&style, ctx, base);
            if inlined != style {
                tag.set("style", inlined);
            }
        }
        out.push_str(&tag.render());
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        // Unique per call: tests run in parallel and several share a name.
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "competition-preview-{name}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("assets")).unwrap();
        dir
    }

    #[test]
    fn base64_matches_the_standard_alphabet() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn inlines_stylesheet_scripts_images_and_css_urls() {
        let dir = scratch("inline");
        fs::write(
            dir.join("index.html"),
            r#"<!doctype html><html><head>
<link rel="stylesheet" href="style.css">
<script src="main.js" defer></script>
<link rel="icon" href="assets/a.png">
</head><body><img src="assets/a.png" alt="x"><a href="other.html">next</a>
<img src="https://example.com/remote.png"></body></html>"#,
        )
        .unwrap();
        fs::write(
            dir.join("style.css"),
            "body{background:url('assets/a.png')}",
        )
        .unwrap();
        fs::write(dir.join("main.js"), "document.body.dataset.ok='1';").unwrap();
        fs::write(dir.join("other.html"), "<p>other</p>").unwrap();
        fs::write(dir.join("assets/a.png"), [0x89, b'P', b'N', b'G']).unwrap();

        let out = write(&dir).unwrap().unwrap();
        assert!(out.issues.is_empty(), "{:?}", out.issues);
        let html = fs::read_to_string(out.page).unwrap();
        assert!(!html.contains("style.css"), "{html}");
        assert!(!html.contains("main.js"), "{html}");
        assert!(!html.contains("\"assets/a.png\""), "{html}");
        assert!(html.contains("<style>body{background:url(\"data:image/png;base64,"));
        assert!(html.contains(r#"<img src="data:image/png;base64,iVBORw==" alt="x">"#));
        assert!(
            html.contains(r#"<a href="../other.html">"#),
            "page links stay links, pointed back at the entry"
        );
        assert!(html.contains("https://example.com/remote.png"));
        // The deferred script stays deferred, its code a data URI.
        let code = base64(b"document.body.dataset.ok='1';");
        assert!(
            html.contains(&format!(
                r#"<script src="data:text/javascript;base64,{code}" defer>"#
            )),
            "{html}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_paths_that_leave_the_snapshot() {
        let dir = scratch("escape");
        let outside = dir.parent().unwrap().join(format!(
            "competition-preview-secret-{}.css",
            std::process::id()
        ));
        fs::write(&outside, "body{}").unwrap();
        let name = outside.file_name().unwrap().to_string_lossy().into_owned();
        fs::write(
            dir.join("index.html"),
            format!(r#"<link rel="stylesheet" href="../{name}"><link rel="stylesheet" href="/etc/hosts">"#),
        )
        .unwrap();
        let preview = write(&dir).unwrap().unwrap();
        assert!(
            preview.issues[0].contains("points outside the entry directory"),
            "{:?}",
            preview.issues
        );
        let html = fs::read_to_string(preview.page).unwrap();
        assert!(html.contains(&format!("../{name}")), "{html}");
        assert!(html.contains("/etc/hosts"));
        let _ = fs::remove_file(outside);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_files_are_reported() {
        let dir = scratch("missing");
        fs::write(
            dir.join("index.html"),
            r#"<img src="assets/gone.png"><img src="assets/gone.png">"#,
        )
        .unwrap();
        let preview = write(&dir).unwrap().unwrap();
        assert_eq!(
            preview.issues,
            vec!["assets/gone.png: file not found".to_owned()]
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_index_html_means_no_preview() {
        let dir = scratch("none");
        fs::write(dir.join("page.html"), "<p>x</p>").unwrap();
        assert_eq!(write(&dir).unwrap(), None);
        let _ = fs::remove_dir_all(&dir);
    }

    fn inline(html: &str) -> String {
        let dir = scratch("unit");
        let ctx = Ctx {
            root: &dir,
            issues: Default::default(),
        };
        let out = inline_html(html, &ctx, Path::new(""));
        let _ = fs::remove_dir_all(&dir);
        out
    }

    #[test]
    fn self_closing_svg_children_stay_self_closing() {
        let html = r#"<svg viewBox="0 0 4 2"><rect x="0" width="1" height="1"/><rect x="2" width="1" height="1" /></svg>"#;
        let out = inline(html);
        assert!(
            out.contains(r#"<rect x="0" width="1" height="1"/>"#),
            "{out}"
        );
        assert!(
            out.contains(r#"<rect x="2" width="1" height="1" />"#),
            "{out}"
        );
    }

    #[test]
    fn character_references_in_attributes_are_kept_as_written() {
        let html = r#"<img alt="Noa&rsquo;s" title='A &mdash; "B"'><input value=&#x2014;>"#;
        assert_eq!(inline(html), html);
    }

    #[test]
    fn remote_deferred_scripts_keep_their_place_before_local_ones() {
        let dir = scratch("order");
        fs::write(dir.join("app.js"), "useLib();").unwrap();
        fs::write(
            dir.join("index.html"),
            r#"<head><script defer src="https://cdn.example/lib.js"></script><script defer src="app.js"></script></head><body><p>x</p></body>"#,
        )
        .unwrap();
        let html = fs::read_to_string(write(&dir).unwrap().unwrap().page).unwrap();
        let lib = html
            .find(r#"<script defer src="https://cdn.example/lib.js">"#)
            .unwrap();
        let app = html
            .find(&format!(
                r#"<script defer src="data:text/javascript;base64,{}">"#,
                base64(b"useLib();")
            ))
            .unwrap();
        // Both stay deferred, in their order: the library first.
        assert!(lib < app, "{html}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn inline_modules_keep_their_order_with_external_ones() {
        let dir = scratch("modules");
        fs::write(dir.join("ready.js"), "globalThis.ready = true;").unwrap();
        fs::write(
            dir.join("index.html"),
            r#"<head><script type="module" src="ready.js"></script><script type="module">use(globalThis.ready);</script></head><body></body>"#,
        )
        .unwrap();
        let html = fs::read_to_string(write(&dir).unwrap().unwrap().page).unwrap();
        let ready = html
            .find(&format!(
                "data:text/javascript;base64,{}",
                base64(b"globalThis.ready = true;")
            ))
            .unwrap();
        let used = html.find("use(globalThis.ready)").unwrap();
        assert!(ready < used, "{html}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn css_imports_are_inlined_and_missing_ones_reported() {
        let dir = scratch("imports");
        fs::write(
            dir.join("index.html"),
            r#"<link rel="stylesheet" href="style.css">"#,
        )
        .unwrap();
        fs::write(
            dir.join("style.css"),
            "@import \"css/layout.css\";\n@import url('gone.css') screen;\nbody{margin:0}",
        )
        .unwrap();
        fs::create_dir_all(dir.join("css")).unwrap();
        fs::write(dir.join("css/layout.css"), ".a{background:url(bg.png)}").unwrap();
        fs::write(dir.join("css/bg.png"), [1, 2, 3]).unwrap();
        let preview = write(&dir).unwrap().unwrap();
        let html = fs::read_to_string(&preview.page).unwrap();
        assert!(!html.contains("@import \"css/layout.css\""), "{html}");
        assert!(
            html.contains(".a{background:url(\"data:image/png;base64,AQID\")}"),
            "{html}"
        );
        assert!(html.contains("body{margin:0}"));
        assert!(
            preview.issues.iter().any(|i| i.starts_with("gone.css")),
            "{:?}",
            preview.issues
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn references_left_as_links_still_point_at_the_entry() {
        let out = inline(
            r##"<a href="other.html">x</a><a href="#top">t</a><img src="assets/gone.png" srcset="a.png 1x, b.png 2x"><a href="https://example.com/">e</a>"##,
        );
        assert!(out.contains(r#"<a href="../other.html">"#), "{out}");
        assert!(out.contains(r##"<a href="#top">"##), "{out}");
        assert!(out.contains(r#"src="../assets/gone.png""#), "{out}");
        assert!(
            out.contains(r#"srcset="../a.png 1x, ../b.png 2x""#),
            "{out}"
        );
        assert!(out.contains(r#"href="https://example.com/""#), "{out}");
    }

    #[test]
    fn a_style_attribute_without_urls_is_kept_as_written() {
        let html = r#"<p style="--q:&#x201C;&nbsp;">x</p>"#;
        assert_eq!(inline(html), html);
    }

    fn css_preview(sheet: &str, files: &[(&str, &str)]) -> (String, Vec<String>) {
        let dir = scratch("css");
        fs::write(
            dir.join("index.html"),
            r#"<link rel="stylesheet" href="style.css">"#,
        )
        .unwrap();
        fs::write(dir.join("style.css"), sheet).unwrap();
        for (name, body) in files {
            fs::write(dir.join(name), body).unwrap();
        }
        let preview = write(&dir).unwrap().unwrap();
        let html = fs::read_to_string(&preview.page).unwrap();
        let _ = fs::remove_dir_all(&dir);
        (html, preview.issues)
    }

    #[test]
    fn imports_after_a_comment_or_charset_are_inlined() {
        let (html, _) = css_preview(
            "/* theme */ @charset \"UTF-8\";\n@import \"layout.css\";\nbody{margin:0}",
            &[("layout.css", ".l{color:red}")],
        );
        assert!(html.contains(".l{color:red}"), "{html}");
        assert!(!html.contains("@import \"layout.css\""), "{html}");
    }

    #[test]
    fn a_semicolon_inside_an_import_url_does_not_end_it() {
        let font =
            "https://fonts.googleapis.com/css2?family=JetBrains+Mono:wght@400;500;700&display=swap";
        let (html, _) = css_preview(&format!("@import url(\"{font}\");\nbody{{margin:0}}"), &[]);
        assert!(html.contains(font), "{html}");
        assert!(!html.contains("500;700&display=swap\");\nbody"), "{html}");
    }

    #[test]
    fn kept_imports_stay_ahead_of_every_inlined_rule() {
        let (html, _) = css_preview(
            "@import \"reset.css\";\n@import url(https://fonts.example/f.css);\nbody{margin:0}",
            &[("reset.css", "*{box-sizing:border-box}")],
        );
        let remote = html.find("https://fonts.example/f.css").unwrap();
        let reset = html.find("*{box-sizing:border-box}").unwrap();
        assert!(remote < reset, "{html}");
    }

    #[test]
    fn a_module_and_a_later_deferred_script_keep_their_order() {
        let dir = scratch("module-defer");
        fs::write(dir.join("app.js"), "init(window.ready);").unwrap();
        fs::write(
            dir.join("index.html"),
            r#"<head><script type="module">window.ready=true</script><script defer src="app.js"></script></head><body><p>x</p></body>"#,
        )
        .unwrap();
        let html = fs::read_to_string(write(&dir).unwrap().unwrap().page).unwrap();
        // The deferred script must not run before the module: it stays a
        // deferred external script (with the code as a data URI), after it.
        let module = html.find("window.ready=true").unwrap();
        let app = html.find(r#"defer src="data:text/javascript;base64,"#);
        assert!(app.is_some_and(|app| module < app), "{html}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_resource_fragment_survives_inlining() {
        let dir = scratch("fragment");
        fs::write(dir.join("sprite.svg"), "<svg/>").unwrap();
        fs::write(dir.join("index.html"), r#"<img src="sprite.svg#detail">"#).unwrap();
        let html = fs::read_to_string(write(&dir).unwrap().unwrap().page).unwrap();
        assert!(html.contains(";base64,PHN2Zy8+#detail\""), "{html}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn data_uris_in_srcset_are_left_whole() {
        let html = r#"<img srcset="data:image/png;base64,AAAA 1x, b.png 2x">"#;
        let out = inline(html);
        assert!(out.contains("data:image/png;base64,AAAA 1x"), "{out}");
        assert!(out.contains("../b.png 2x"), "{out}");
    }

    #[test]
    fn script_sources_left_as_links_point_back_at_the_entry() {
        let out =
            inline(r#"<script src="missing.js"></script><script defer src="gone.js"></script>"#);
        assert!(out.contains(r#"src="../missing.js""#), "{out}");
        assert!(out.contains(r#"src="../gone.js""#), "{out}");
    }

    #[test]
    fn inlined_script_cannot_close_its_element() {
        assert_eq!(
            escape_raw_text("a='</script><b>'", "script"),
            "a='<\\/script><b>'"
        );
    }
}
