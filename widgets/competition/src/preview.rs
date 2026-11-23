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
pub const INLINE_RULES: &str = "Inlined from the root `index.html`: `<link rel=\"stylesheet\">`, `<script src>` (deferred and module scripts move to the end of `<body>`), `src`/`href`/`poster` resources such as images, icons and fonts, and `url(...)` in stylesheets and `style` attributes, each when it is a relative path to a file of at most 8 MiB inside the entry directory. Not inlined: `srcset`, URLs built by scripts, `fetch`/XHR, other pages (links stay links), and absolute or remote URLs.";

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

/// Rewrites every local `url(...)` in a stylesheet to a data URI. `base` is
/// the stylesheet's directory relative to `root`.
fn inline_css(css: &str, ctx: &Ctx, base: &Path) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
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
                out.push('"');
            }
            None => out.push_str(raw),
        }
        rest = &tail[close..];
    }
    out.push_str(rest);
    out
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
    // Deferred and module scripts run after parsing; inline copies go just
    // before `</body>` so they still run once the DOM exists.
    let mut deferred = Vec::new();
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
        if rest.len() >= 6 && rest.as_bytes()[..6].eq_ignore_ascii_case(b"</body") {
            out.extend(deferred.drain(..));
            out.push_str("</");
            rest = &rest[2..];
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
            // A deferred or module script with `src` runs after parsing, in
            // document order. Inlined copies run where they stand, so every
            // such script, local or remote, moves before `</body>` without
            // `defer`, keeping the order among them.
            let late = name == "script"
                && tag.has("src")
                && !tag.has("async")
                && (tag.has("defer") || tag.get("type") == Some("module"));
            let local = (name == "script")
                .then(|| tag.get("src").and_then(|src| resolve(ctx, base, src)))
                .flatten()
                .and_then(|file| fs::read_to_string(file).ok());
            if let Some(code) = local {
                tag.remove(&["src", "defer", "async", "integrity", "crossorigin"]);
                let inlined = format!("{}{}", tag.render(), escape_raw_text(&code, "script"));
                if late {
                    deferred.push(format!("{inlined}</script>"));
                    // Skip the original end tag: the copy carries its own.
                    if let Some(end) = rest.find('>') {
                        rest = &rest[end + 1..];
                    }
                } else {
                    out.push_str(&inlined);
                }
                continue;
            }
            if late {
                tag.remove(&["defer"]);
                deferred.push(format!("{}{body}</script>", tag.render()));
                if let Some(end) = rest.find('>') {
                    rest = &rest[end + 1..];
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
        let navigates = name == "a" || name == "area" || name == "form";
        if !navigates {
            for attr in URL_ATTRS {
                if let Some(uri) = tag
                    .get(attr)
                    .and_then(|v| resolve(ctx, base, v))
                    .filter(|p| !is_page(p))
                    .and_then(|p| data_uri(&p))
                {
                    tag.set(attr, uri);
                }
            }
        }
        if let Some(style) = tag.get("style").map(str::to_owned) {
            tag.set("style", inline_css(&style, ctx, base));
        }
        out.push_str(&tag.render());
    }
    out.push_str(rest);
    out.extend(deferred.drain(..));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("competition-preview-{name}-{}", std::process::id()));
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
            html.contains(r#"<a href="other.html">"#),
            "page links stay links"
        );
        assert!(html.contains("https://example.com/remote.png"));
        // The deferred script moves to the end of <body>, after the DOM it reads.
        let script = html.find("document.body.dataset.ok").unwrap();
        let img = html.find("<img src=\"data:").unwrap();
        assert!(script > img, "{html}");
        assert!(html.find("</body>").unwrap() > script);
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
        let lib = html.find("https://cdn.example/lib.js").unwrap();
        let app = html.find("useLib();").unwrap();
        let body = html.find("<p>x</p>").unwrap();
        // Both run after the document, the library first.
        assert!(body < lib && lib < app, "{html}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn inlined_script_cannot_close_its_element() {
        assert_eq!(
            escape_raw_text("a='</script><b>'", "script"),
            "a='<\\/script><b>'"
        );
    }
}
