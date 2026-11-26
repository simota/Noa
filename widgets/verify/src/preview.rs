//! A local, self-contained comparison of stored image evidence. The UI
//! runs this on a worker: every input is read without following links and
//! checked against its recorded size, digest and media before embedding.

use std::ffi::{CString, OsStr};
use std::fmt::Write as _;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::capture::digest_bytes;
use crate::model::types::{Evidence, EvidenceBody, MAX_IMAGE_BYTES, Media};
use crate::{evidence, fsx, media};

pub const MAX_IMAGES: usize = 12;
pub const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
const FILE: &str = "image-comparison.html";
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// Captured view metadata. Criteria are supplied by the UI's existing
/// evidence relation rules, so the comparison agrees with the gallery.
pub struct Image<'a> {
    pub evidence: &'a Evidence,
    pub criteria: Vec<&'a str>,
}

/// Prepares one replaceable page inside `session_dir`. That directory's
/// parent is trusted, just like the session store's root; its final
/// component and all stored evidence paths must be real directories/files.
/// The caller publishes it only while its session is still on screen.
pub fn prepare(
    reply_base: &Path,
    reply_dir: &Path,
    session_dir: &Path,
    title: &str,
    images: &[Image<'_>],
) -> Result<Prepared, String> {
    let html = render(reply_base, reply_dir, title, images)?;
    prepare_file(session_dir, html.as_bytes())
        .map_err(|e| format!("画像比較ページを保存できません: {e}"))
}

/// Owns an unpublished temporary file. Dropping a cancelled job or an
/// undeliverable answer removes it, without touching the previous page.
pub struct Prepared {
    directory: OwnedFd,
    temporary: CString,
    output: PathBuf,
}

impl Prepared {
    /// The UI calls this after checking the captured session. The bytes
    /// are already written and synced; only the atomic rename remains.
    pub fn publish(self) -> Result<PathBuf, String> {
        let name = CString::new(FILE).expect("fixed filename has no NUL");
        // SAFETY: both names are C strings relative to the same open directory;
        // rename replaces a destination link itself, never its target.
        let renamed = unsafe {
            libc::renameat(
                self.directory.as_raw_fd(),
                self.temporary.as_ptr(),
                self.directory.as_raw_fd(),
                name.as_ptr(),
            )
        };
        if renamed != 0 {
            return Err(format!(
                "画像比較ページを更新できません: {}",
                io::Error::last_os_error()
            ));
        }
        Ok(self.output.clone())
    }
}

impl Drop for Prepared {
    fn drop(&mut self) {
        // SAFETY: the temporary name is relative to its open directory.
        // After publish there is no file at this name, so this is a no-op.
        unsafe { libc::unlinkat(self.directory.as_raw_fd(), self.temporary.as_ptr(), 0) };
    }
}

fn render(
    reply_base: &Path,
    reply_dir: &Path,
    title: &str,
    images: &[Image<'_>],
) -> Result<String, String> {
    let mut cards = String::new();
    let mut omitted = Vec::new();
    let mut total = 0;
    let mut count = 0;
    for image in images.iter().take(MAX_IMAGES) {
        let item = image.evidence;
        let (bytes, format) = match checked_image(reply_base, reply_dir, item, total) {
            Ok(found) => found,
            Err(why) => {
                omitted.push(format!("{}: {why}", item.key));
                continue;
            }
        };
        total += bytes.len() as u64;
        count += 1;
        let caption = match &item.body {
            EvidenceBody::File { caption, .. } => caption.as_deref().unwrap_or("説明なし"),
            _ => unreachable!("checked_image accepts only image files"),
        };
        let (owner, round) = match item.key.split() {
            Some((part, round, _)) => (part.to_string(), round),
            None => (
                "あなた".to_owned(),
                item.attached.map_or(0, |attachment| attachment.round),
            ),
        };
        let criteria = if image.criteria.is_empty() {
            "対応する基準なし".to_owned()
        } else {
            image.criteria.join(" · ")
        };
        let _ = write!(
            cards,
            "<figure id=\"image-{count}\"><figcaption><h2>{}</h2>\
             <p>{}</p><p class=\"meta\">{} · ラウンド {round} · {} · {} バイト</p>\
             <p class=\"meta\">{}</p>\
             <a class=\"zoom\" href=\"#image-{count}\">拡大</a> \
             <a class=\"back\" href=\"#overview\">一覧へ戻る</a></figcaption>\
             <div class=\"picture\"><img alt=\"{}\" src=\"data:{};base64,",
            escape(item.key.as_str()),
            escape(caption),
            escape(&owner),
            format.name(),
            bytes.len(),
            escape(&criteria),
            escape(caption),
            mime(format).expect("checked image format"),
        );
        base64(&bytes, &mut cards);
        cards.push_str("\"></div></figure>");
    }
    if images.len() > MAX_IMAGES {
        omitted.push(format!(
            "先頭 {MAX_IMAGES} 枚を処理しました。残り {} 枚は上限により省略しました。条件を絞って再度開いてください。",
            images.len() - MAX_IMAGES
        ));
    }
    if count == 0 {
        return Err(if omitted.is_empty() {
            "比較できる保存済み画像がありません".to_owned()
        } else {
            format!("比較できる画像がありません: {}", omitted.join(" / "))
        });
    }
    let mut html = format!(
        "<!doctype html><html lang=\"ja\"><head><meta charset=\"utf-8\">\
         <meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src data:; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>画像比較 — {}</title><style>{STYLE}</style></head>\
         <body id=\"overview\"><header><p class=\"meta\">Noa Verify · 保存済み画像</p>\
         <h1>{}</h1><p>{count} 枚 / 対象 {} 枚 · {total} バイト</p>\
         <p>「拡大」で画像を原寸まで表示できます。ブラウザのズーム操作も使えます。</p>\
         <p class=\"meta\">保存時のサイズ・ハッシュと画像形式を照合済みです。\
         画像はこのファイルに埋め込まれ、外部への通信は行いません。\
         比較ページは次に開くときに置き換わります。</p></header>",
        escape(title),
        escape(title),
        images.len(),
    );
    if !omitted.is_empty() {
        html.push_str("<aside aria-label=\"省略した画像\"><h2>表示しなかった画像</h2><ul>");
        for why in omitted {
            let _ = write!(html, "<li>{}</li>", escape(&why));
        }
        html.push_str("</ul></aside>");
    }
    html.push_str("<main>");
    html.push_str(&cards);
    html.push_str("</main></body></html>");
    Ok(html)
}

fn checked_image(
    reply_base: &Path,
    reply_dir: &Path,
    item: &Evidence,
    total: u64,
) -> Result<(Vec<u8>, Media), String> {
    let format = item
        .media()
        .filter(|m| mime(*m).is_some())
        .ok_or("画像ではありません")?;
    let stored = item.stored.as_ref().ok_or("保存されたコピーがありません")?;
    if stored.bytes > MAX_IMAGE_BYTES {
        return Err("1 枚の上限（15 MiB）を超えています".to_owned());
    }
    if stored.bytes > MAX_TOTAL_BYTES.saturating_sub(total) {
        return Err("合計サイズの上限（64 MiB）を超えています".to_owned());
    }
    let (bytes, over) = evidence::read_stored(reply_base, reply_dir, stored, stored.bytes)
        .map_err(|e| format!("保存済み画像を読めません: {e}"))?;
    if over || bytes.len() as u64 != stored.bytes || digest_bytes(&bytes) != stored.digest {
        return Err("保存時のサイズ・ハッシュと一致しません".to_owned());
    }
    if media::sniff(&bytes) != Some(format) {
        return Err("保存時の画像形式と一致しません".to_owned());
    }
    Ok((bytes, format))
}

fn mime(format: Media) -> Option<&'static str> {
    Some(match format {
        Media::Png => "image/png",
        Media::Jpeg => "image/jpeg",
        Media::Gif => "image/gif",
        Media::Webp => "image/webp",
        Media::Mp4 | Media::QuickTime | Media::Webm => return None,
    })
}

fn escape(text: &str) -> String {
    let mut safe = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => safe.push_str("&amp;"),
            '<' => safe.push_str("&lt;"),
            '>' => safe.push_str("&gt;"),
            '"' => safe.push_str("&quot;"),
            '\'' => safe.push_str("&#39;"),
            _ => safe.push(c),
        }
    }
    safe
}

/// RFC 4648's standard alphabet; no wrapping or untrusted MIME/path is
/// interpolated into the generated data URL.
fn base64(bytes: &[u8], out: &mut String) {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    out.reserve(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        out.push(ALPHABET[(a >> 2) as usize] as char);
        out.push(ALPHABET[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(((b & 15) << 2) | (c >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(c & 63) as usize] as char
        } else {
            '='
        });
    }
}

fn prepare_file(session_dir: &Path, bytes: &[u8]) -> io::Result<Prepared> {
    // `--session` also accepts a relative directory. Keep the destination
    // absolute for the OS opener without canonicalizing through any link.
    let session_dir = if session_dir.is_absolute() {
        session_dir.to_owned()
    } else {
        std::env::current_dir()?.join(session_dir)
    };
    let base = session_dir
        .parent()
        .ok_or_else(|| io::Error::other("no session parent"))?;
    let dir = fsx::open_dir(base, &session_dir, false)?;
    let serial = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    let tmp = format!(".{FILE}.{}.{serial}.tmp", std::process::id());
    fsx::write_new_at(&dir, OsStr::new(&tmp), bytes)?;
    Ok(Prepared {
        directory: dir,
        temporary: CString::new(tmp).expect("generated filename has no NUL"),
        output: session_dir.join(FILE),
    })
}

const STYLE: &str = r#"
:root{color-scheme:light dark;font-family:system-ui,sans-serif;color:#e4e8ef;background:#151a22}
*{box-sizing:border-box}body{margin:0;padding:24px;line-height:1.6}header,aside{max-width:1100px;margin:0 auto 24px}
h1{font-size:1.5rem;margin:8px 0}h2{font-size:1rem;margin:0}p{margin:6px 0;overflow-wrap:anywhere}.meta{color:#abb8c9;font-size:.9rem}
main{display:grid;grid-template-columns:repeat(auto-fit,minmax(min(100%,400px),1fr));gap:20px}
figure{margin:0;min-width:0;border:1px solid #455166;border-radius:12px;overflow:hidden;background:#202836}
figcaption{padding:16px;overflow-wrap:anywhere}a{color:#a7c8ff}a:focus-visible{outline:3px solid #e9b85f;outline-offset:3px}
.picture{overflow:auto;background:repeating-conic-gradient(#ccd0d4 0% 25%,#f4f5f6 0% 50%) 50%/20px 20px;padding:12px;text-align:center}
img{display:block;max-width:100%;max-height:55vh;width:auto;height:auto;margin:auto;object-fit:contain}
.back{display:none}figure:target{grid-column:1/-1;border:2px solid #a7c8ff}figure:target img{max-height:none;max-width:none}
figure:target .picture{max-height:85vh}figure:target .back{display:inline}figure:target .zoom{display:none}
aside{border-left:4px solid #e9b85f;padding:12px 18px;background:#322d23}li{overflow-wrap:anywhere}
@media(prefers-color-scheme:light){:root{color:#172334;background:#f5f7fa}figure{background:#fff;border-color:#b7c3d2}.meta{color:#52647b}a{color:#174c98}aside{background:#fff1d7}figure:target{border-color:#174c98}}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::types::{EvidenceKey, Stored};
    use std::fs;
    use std::os::unix::fs::symlink;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("noa-verify-preview-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn image(dir: &Path, name: &str, bytes: &[u8]) -> Evidence {
        fs::write(dir.join(name), bytes).unwrap();
        Evidence {
            key: EvidenceKey::new(format!("codex/r1/{name}")),
            body: EvidenceBody::File {
                path: name.to_owned(),
                caption: Some("<script>alert('x')</script> & \"caption\"".to_owned()),
                media: Some(Media::Png),
                marks: vec![],
                shows: vec!["AC-1".to_owned()],
            },
            stored: Some(Stored {
                path: name.into(),
                bytes: bytes.len() as u64,
                digest: digest_bytes(bytes),
            }),
            rerun: None,
            attached: None,
        }
    }

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nfixture";

    #[test]
    fn base64_matches_standard_vectors() {
        for (bytes, expected) in [
            (&b""[..], ""),
            (&b"f"[..], "Zg=="),
            (&b"fo"[..], "Zm8="),
            (&b"foo"[..], "Zm9v"),
            (&b"foob"[..], "Zm9vYg=="),
            (&b"fooba"[..], "Zm9vYmE="),
            (&b"foobar"[..], "Zm9vYmFy"),
            (&b"\xfb\xff\xff"[..], "+///"),
        ] {
            let mut encoded = String::new();
            base64(bytes, &mut encoded);
            assert_eq!(encoded, expected);
        }
    }

    #[test]
    fn only_intact_saved_images_are_embedded_and_text_is_escaped() {
        let dir = temp("integrity");
        let good = image(&dir, "good.png", PNG);
        let changed = image(&dir, "changed.png", PNG);
        fs::write(dir.join("changed.png"), b"\x89PNG\r\n\x1a\nchanged").unwrap();
        let wrong_format = image(&dir, "wrong.png", b"GIF89a");
        let mut too_large = good.clone();
        too_large.stored.as_mut().unwrap().bytes = MAX_IMAGE_BYTES + 1;
        let items = [&good, &changed, &wrong_format, &too_large].map(|evidence| Image {
            evidence,
            criteria: vec!["AC-1"],
        });
        let page = prepare(&dir, &dir, &dir, "<target>", &items)
            .unwrap()
            .publish()
            .unwrap();
        let html = fs::read_to_string(page).unwrap();
        assert_eq!(html.matches("<img ").count(), 1);
        assert!(html.contains("data:image/png;base64,"));
        assert!(html.contains("default-src 'none'"));
        assert!(html.contains("&lt;target&gt;"));
        assert!(
            html.contains(
                "&lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt; &amp; &quot;caption&quot;"
            )
        );
        assert!(!html.contains("<script>"));
        assert!(html.contains("サイズ・ハッシュと一致しません"));
        assert!(html.contains("画像形式と一致しません"));
        assert!(html.contains("15 MiB"));
        assert!(
            checked_image(&dir, &dir, &good, MAX_TOTAL_BYTES)
                .unwrap_err()
                .contains("64 MiB")
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn input_links_are_refused_and_replacement_does_not_follow_output_links() {
        let base = temp("links");
        let session = base.join("session");
        fs::create_dir(&session).unwrap();
        let mut item = image(&base, "image.png", PNG);
        symlink(base.join("image.png"), base.join("linked.png")).unwrap();
        item.stored.as_mut().unwrap().path = "linked.png".into();
        assert!(checked_image(&base, &base, &item, 0).is_err());
        item.stored.as_mut().unwrap().path = "../image.png".into();
        assert!(checked_image(&base, &base, &item, 0).is_err());
        symlink(&base, base.join("linked-dir")).unwrap();
        item.stored.as_mut().unwrap().path = "linked-dir/image.png".into();
        assert!(checked_image(&base, &base, &item, 0).is_err());
        let sentinel = base.join("sentinel");
        fs::write(&sentinel, b"untouched").unwrap();
        symlink(&sentinel, session.join(FILE)).unwrap();
        let path = prepare_file(&session, b"first").unwrap().publish().unwrap();
        assert_eq!(fs::read(&sentinel).unwrap(), b"untouched");
        assert!(
            !fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let old = prepare_file(&session, b"cancelled").unwrap();
        assert_eq!(
            prepare_file(&session, b"second")
                .unwrap()
                .publish()
                .unwrap(),
            path
        );
        drop(old);
        assert_eq!(fs::read(path).unwrap(), b"second");
        assert_eq!(fs::read_dir(&session).unwrap().count(), 1);
        symlink(&session, base.join("linked-session")).unwrap();
        assert!(prepare_file(&base.join("linked-session"), b"refused").is_err());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn count_limit_is_explicit_and_no_empty_page_is_written() {
        let dir = temp("limit");
        let item = image(&dir, "image.png", PNG);
        let images: Vec<_> = (0..MAX_IMAGES + 1)
            .map(|_| Image {
                evidence: &item,
                criteria: vec![],
            })
            .collect();
        let html = render(&dir, &dir, "limits", &images).unwrap();
        assert_eq!(html.matches("<img ").count(), MAX_IMAGES);
        assert!(html.contains("残り 1 枚は上限により省略"));
        assert!(prepare(&dir, &dir, &dir, "empty", &[]).is_err());
        assert!(!dir.join(FILE).exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
